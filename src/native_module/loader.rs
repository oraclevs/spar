//! Native module loader: dlopen, descriptor validation, capability negotiation, registration of the
//! module's functions into the ordinary `NativeRegistry`.
//!
//! Policy: libraries are never unloaded (finalizers, worker threads and callbacks may still point
//! into them). `shutdown_all` runs `quiesce`/`destroy` hooks but leaves the code mapped.

use std::collections::HashMap;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use spar_native_sys::*;

use super::env::CallEnv;
use super::host;
use crate::ast::SparType;
use crate::async_runtime::RuntimeFault;
use crate::error::{Span, SparError};
use crate::runtime::{NativeFunction, NativeRegistry, RuntimeContext, Value};

/// Capabilities this runtime implements. Grows as features land; never shrinks within an ABI major.
pub const RUNTIME_CAPABILITIES: u64 = SPAR_CAP_STRINGS
    | SPAR_CAP_BYTES
    | SPAR_CAP_ZERO_COPY_BYTES
    | SPAR_CAP_TYPED_ARRAYS
    | SPAR_CAP_LISTS
    | SPAR_CAP_RECORDS
    | SPAR_CAP_NATIVE_RESOURCES
    | SPAR_CAP_CALLBACKS
    | SPAR_CAP_ASYNC
    | SPAR_CAP_DIRECT_CALLS;

#[derive(Debug)]
pub enum NativeLoadError {
    Open { path: PathBuf, message: String },
    MissingSymbol { path: PathBuf },
    Descriptor { path: PathBuf, message: String },
    Init { path: PathBuf, status: i32, message: String },
    Registration { path: PathBuf, message: String },
}

impl std::fmt::Display for NativeLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Open { path, message } => write!(f, "cannot load native module '{}': {message}", path.display()),
            Self::MissingSymbol { path } => write!(
                f,
                "'{}' is not a Spar native module: it does not export '{}'",
                path.display(),
                "spar_native_module_v0"
            ),
            Self::Descriptor { path, message } => write!(f, "native module '{}' rejected: {message}", path.display()),
            Self::Init { path, status, message } => {
                write!(f, "native module '{}' failed to initialise (status {status}): {message}", path.display())
            }
            Self::Registration { path, message } => write!(f, "native module '{}': {message}", path.display()),
        }
    }
}

impl std::error::Error for NativeLoadError {}

impl From<NativeLoadError> for SparError {
    fn from(e: NativeLoadError) -> Self {
        SparError::EvalError { message: e.to_string(), span: Span::dummy() }
    }
}

#[derive(Debug, Clone)]
pub struct ModuleInfo {
    pub name: String,
    pub version: (u32, u32, u32),
    pub path: PathBuf,
    pub abi_major: u16,
    pub min_abi_minor: u16,
    pub required_capabilities: u64,
    pub optional_capabilities: u64,
    pub target: String,
    pub functions: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RetKind {
    Any,
    Void,
    Int,
    Float,
    Bool,
    Str,
}

fn ret_kind(ty: &SparType) -> RetKind {
    match ty {
        SparType::Void => RetKind::Void,
        SparType::Int => RetKind::Int,
        SparType::Float => RetKind::Float,
        SparType::Bool => RetKind::Bool,
        SparType::Str => RetKind::Str,
        _ => RetKind::Any,
    }
}

pub(crate) struct FnInfo {
    module: String,
    name: String,
    invoke: SparNativeFn,
    userdata: *mut c_void,
    ret: RetKind,
    argc: usize,
    /// Index into `EXTERNALS` for functions that need interpreter access (`SPAR_FN_CALLS`).
    external: Option<u32>,
    is_async: bool,
    direct: Option<Arc<super::direct::DirectFn>>,
}
// SAFETY: `userdata` is module-owned state; the ABI requires module functions to be callable from
// any thread that owns a call env (natives run on scheduler threads).
unsafe impl Send for FnInfo {}
unsafe impl Sync for FnInfo {}

struct FunctionDef {
    name: String,
    params: Vec<(String, SparType)>,
    ret: SparType,
    info: Arc<FnInfo>,
}

pub(super) struct ModuleBuilder {
    name: String,
    functions: Vec<FunctionDef>,
    error: Option<String>,
}

pub struct LoadedModule {
    info: ModuleInfo,
    functions: Vec<FunctionDef>,
    state: *mut c_void,
    quiesce: Option<SparModuleQuiesceFn>,
    destroy: Option<SparModuleDestroyFn>,
    shut_down: Mutex<bool>,
    _lib: &'static libloading::Library,
}
// SAFETY: state pointer is only handed back to the module's own hooks.
unsafe impl Send for LoadedModule {}
unsafe impl Sync for LoadedModule {}

impl std::fmt::Debug for LoadedModule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedModule").field("info", &self.info).finish()
    }
}

/// Functions that run through the interpreter (`NativeIntrinsic::External(index)`).
static EXTERNALS: Mutex<Vec<Option<Arc<FnInfo>>>> = Mutex::new(Vec::new());

static LOADED: Mutex<Vec<(PathBuf, Arc<LoadedModule>)>> = Mutex::new(Vec::new());

fn debug_enabled() -> bool {
    std::env::var_os("SPAR_NATIVE_DEBUG").is_some()
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

unsafe fn text(ptr: *const u8, len: u64) -> Result<String, String> {
    if len == 0 {
        return Ok(String::new());
    }
    if ptr.is_null() || len > 1 << 20 {
        return Err("invalid string pointer or length".into());
    }
    String::from_utf8(std::slice::from_raw_parts(ptr, len as usize).to_vec()).map_err(|_| "string is not UTF-8".into())
}

pub(super) unsafe extern "C" fn module_add_function(module: *mut SparModule, spec: *const SparFunctionSpec) -> spar_status_t {
    if module.is_null() || spec.is_null() {
        return SPAR_E_INVALID_ARGUMENT;
    }
    let builder = &mut *(module as *mut ModuleBuilder);
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| add_function(builder, &*spec))) {
        Ok(Ok(())) => SPAR_OK,
        Ok(Err((status, msg))) => {
            builder.error.get_or_insert(msg);
            status
        }
        Err(_) => SPAR_E_PANIC,
    }
}

unsafe fn add_function(builder: &mut ModuleBuilder, spec: &SparFunctionSpec) -> Result<(), (i32, String)> {
    let bad = |m: String| (SPAR_E_INVALID_ARGUMENT, m);
    if (spec.struct_size as usize) < std::mem::size_of::<SparFunctionSpec>() {
        return Err((SPAR_E_ABI_MISMATCH, format!("function spec struct_size {} is too small", spec.struct_size)));
    }
    let name = text(spec.name, spec.name_len).map_err(bad)?;
    if !is_identifier(&name) {
        return Err(bad(format!("'{name}' is not a valid Spar function name")));
    }
    if builder.functions.iter().any(|f| f.name == name) {
        return Err(bad(format!("function '{name}' registered twice")));
    }
    let has_direct = !spec.direct.is_null();
    if has_direct && spec.flags & (SPAR_FN_ASYNC | SPAR_FN_CALLS) != 0 {
        return Err(bad(format!("'{name}': direct signatures cannot be async or call back into Spar")));
    }
    let invoke = match spec.invoke {
        Some(f) => f,
        None if has_direct => unreachable_invoke,
        None => return Err(bad(format!("'{name}': missing invoke entry"))),
    };
    if spec.param_count > 0 && spec.params.is_null() {
        return Err(bad(format!("'{name}': null params")));
    }
    if spec.param_count > 64 {
        return Err(bad(format!("'{name}': too many parameters")));
    }
    let mut params = Vec::new();
    for i in 0..spec.param_count as usize {
        let p = &*spec.params.add(i);
        let pname = text(p.name, p.name_len).map_err(bad)?;
        let ptype = text(p.type_, p.type_len).map_err(bad)?;
        if !is_identifier(&pname) {
            return Err(bad(format!("'{name}': bad parameter name '{pname}'")));
        }
        let ty = crate::parser::Parser::parse_type_text(&ptype)
            .map_err(|e| bad(format!("'{name}': parameter '{pname}' has invalid type '{ptype}': {e}")))?;
        params.push((pname, ty));
    }
    let ret_text = text(spec.ret_type, spec.ret_type_len).map_err(bad)?;
    let ret = if ret_text.is_empty() || ret_text == "void" {
        SparType::Void
    } else {
        crate::parser::Parser::parse_type_text(&ret_text)
            .map_err(|e| bad(format!("'{name}': invalid return type '{ret_text}': {e}")))?
    };
    let direct = if has_direct {
        let sig = text(spec.direct_sig, spec.direct_sig_len).map_err(bad)?;
        let (arg_letters, ret_letter) =
            sig.split_once('>').ok_or_else(|| bad(format!("'{name}': direct signature '{sig}' must look like 'ii>i'")))?;
        if arg_letters.len() != params.len() || ret_letter.len() != 1 {
            return Err(bad(format!("'{name}': direct signature '{sig}' does not match its {} parameters", params.len())));
        }
        for (i, letter) in arg_letters.bytes().enumerate() {
            if letter == b'v' || !super::direct::letter_matches(letter, &params[i].1) {
                return Err(bad(format!("'{name}': direct signature letter '{}' does not match parameter '{}' of type {:?}", letter as char, params[i].0, params[i].1)));
            }
        }
        if !super::direct::letter_matches(ret_letter.as_bytes()[0], &ret) {
            return Err(bad(format!("'{name}': direct return letter '{ret_letter}' does not match declared return type {ret:?}")));
        }
        Some(Arc::new(
            super::direct::build(spec.direct, &sig, format!("{}::{name}", builder.name))
                .ok_or_else(|| bad(format!("'{name}': unsupported direct signature '{sig}' (max 4 arguments of i/f/b)")))?,
        ))
    } else {
        None
    };
    if spec.flags & SPAR_FN_ASYNC != 0
        && !matches!(&ret, SparType::Applied { name, arguments } if name == "Promise" && arguments.len() == 1)
    {
        return Err(bad(format!("'{name}': async functions must declare a `Promise<T>` return type, got '{ret_text}'")));
    }
    let info = Arc::new(FnInfo {
        module: builder.name.clone(),
        name: name.clone(),
        invoke,
        userdata: spec.userdata,
        ret: ret_kind(&ret),
        argc: params.len(),
        is_async: spec.flags & SPAR_FN_ASYNC != 0,
        direct,
        external: if spec.flags & (SPAR_FN_CALLS | SPAR_FN_ASYNC) != 0 {
            let mut ext = EXTERNALS.lock().unwrap();
            ext.push(None);
            Some((ext.len() - 1) as u32)
        } else {
            None
        },
    });
    if let Some(index) = info.external {
        EXTERNALS.lock().unwrap()[index as usize] = Some(info.clone());
    }
    builder.functions.push(FunctionDef { name, params, ret, info });
    Ok(())
}

/// Placeholder `invoke` for direct-only functions; never reached (direct adapters bypass it).
unsafe extern "C" fn unreachable_invoke(_e: *mut SparEnv, _u: *mut c_void, _a: *const SparValue, _n: u64, _o: *mut SparValue) -> spar_status_t {
    SPAR_E_UNSUPPORTED
}

static API: SparApiV0 = SparApiV0 {
    struct_size: std::mem::size_of::<SparApiV0>() as u32,
    abi_major: SPAR_NATIVE_ABI_MAJOR,
    abi_minor: SPAR_NATIVE_ABI_MINOR,
    capabilities: RUNTIME_CAPABILITIES,
    module_add_function: Some(module_add_function),
    error_set: Some(host::error_set),
    int_get: Some(host::int_get),
    float_get: Some(host::float_get),
    bool_get: Some(host::bool_get),
    string_new: Some(host::string_new),
    string_view: Some(host::string_view),
    bytes_new: Some(host::bytes_new),
    buffer_borrow: Some(host::buffer_borrow),
    buffer_release: Some(host::buffer_release),
    buffer_new: Some(host::buffer_new),
    buffer_from_external: Some(host::buffer_from_external),
    buffer_to_list: Some(host::buffer_to_list),
    list_len: Some(host::list_len),
    list_get: Some(host::list_get),
    list_new: Some(host::list_new),
    list_push: Some(host::list_push),
    symbol_intern: Some(host::symbol_intern),
    record_new: Some(host::record_new),
    record_set: Some(host::record_set),
    record_get: Some(host::record_get),
    record_len: Some(host::record_len),
    option_some: Some(host::option_some),
    option_none: Some(host::option_none),
    option_get: Some(host::option_get),
    ref_new: Some(host::ref_new),
    ref_get: Some(host::ref_get),
    ref_drop: Some(host::ref_drop),
    resource_new: Some(host::resource_new),
    resource_get: Some(host::resource_get),
    resource_close: Some(host::resource_close),
    call: Some(host::call),
    async_begin: Some(super::async_op::async_begin_api),
    async_complete: Some(super::async_op::async_complete),
    async_fail: Some(super::async_op::async_fail),
    async_is_cancelled: Some(super::async_op::async_is_cancelled),
    async_release: Some(super::async_op::async_release),
};

/// The API table given to modules (also used by tests that load modules built in-process).
pub fn api_table() -> &'static SparApiV0 {
    &API
}

fn validate_descriptor(path: &Path, d: &SparModuleDescriptor) -> Result<(), NativeLoadError> {
    let err = |m: String| NativeLoadError::Descriptor { path: path.to_path_buf(), message: m };
    if (d.struct_size as usize) < std::mem::size_of::<SparModuleDescriptor>() {
        return Err(err(format!(
            "descriptor struct_size {} is smaller than the {} bytes this runtime expects (ABI {}.{})",
            d.struct_size,
            std::mem::size_of::<SparModuleDescriptor>(),
            SPAR_NATIVE_ABI_MAJOR,
            SPAR_NATIVE_ABI_MINOR
        )));
    }
    if d.abi_major != SPAR_NATIVE_ABI_MAJOR {
        return Err(err(format!("module targets ABI major {}, runtime provides {}", d.abi_major, SPAR_NATIVE_ABI_MAJOR)));
    }
    if d.min_abi_minor > SPAR_NATIVE_ABI_MINOR {
        return Err(err(format!(
            "module needs ABI {}.{}, runtime provides {}.{}",
            d.abi_major, d.min_abi_minor, SPAR_NATIVE_ABI_MAJOR, SPAR_NATIVE_ABI_MINOR
        )));
    }
    let missing = d.required_capabilities & !RUNTIME_CAPABILITIES;
    if missing != 0 {
        return Err(err(format!(
            "module requires capabilities 0x{missing:x} that this runtime does not provide (runtime: 0x{RUNTIME_CAPABILITIES:x})"
        )));
    }
    if d.init.is_none() {
        return Err(err("descriptor has no init function".into()));
    }
    Ok(())
}

fn target_matches(target: &str) -> bool {
    target.is_empty() || (target.starts_with(std::env::consts::ARCH) && target.contains(std::env::consts::OS))
}

/// Loads (once per canonical path) and initialises a native module.
pub fn load_module(path: &Path) -> Result<Arc<LoadedModule>, NativeLoadError> {
    let canonical = path
        .canonicalize()
        .map_err(|e| NativeLoadError::Open { path: path.to_path_buf(), message: e.to_string() })?;
    {
        let loaded = LOADED.lock().unwrap();
        if let Some((_, m)) = loaded.iter().find(|(p, _)| *p == canonical) {
            return Ok(m.clone());
        }
    }
    // SAFETY: loading a shared library runs its constructors; native modules are trusted code
    // (documented security boundary).
    let lib = unsafe { libloading::Library::new(&canonical) }
        .map_err(|e| NativeLoadError::Open { path: canonical.clone(), message: e.to_string() })?;
    let lib: &'static libloading::Library = Box::leak(Box::new(lib));
    let entry: libloading::Symbol<'static, unsafe extern "C" fn() -> *const SparModuleDescriptor> =
        unsafe { lib.get(SPAR_MODULE_SYMBOL) }.map_err(|_| NativeLoadError::MissingSymbol { path: canonical.clone() })?;
    // SAFETY: the symbol has the documented signature; a wrong-signature export is a module bug.
    let raw = unsafe { entry() };
    if raw.is_null() {
        return Err(NativeLoadError::Descriptor { path: canonical, message: "entry returned a null descriptor".into() });
    }
    // Read only the prefix the module declared, zero-padding the rest so older modules stay loadable
    // once the descriptor grows.
    let declared = unsafe { (*raw).struct_size } as usize;
    let mut desc: SparModuleDescriptor = unsafe { std::mem::zeroed() };
    unsafe {
        std::ptr::copy_nonoverlapping(
            raw as *const u8,
            &mut desc as *mut _ as *mut u8,
            declared.min(std::mem::size_of::<SparModuleDescriptor>()),
        );
    }
    let short = declared < std::mem::size_of::<SparModuleDescriptor>();
    if short && declared >= 8 {
        // A prefix shorter than we know is rejected below with a clear message.
    }
    validate_descriptor(&canonical, &desc)?;
    let name = unsafe { text(desc.module_name, desc.module_name_len) }
        .map_err(|m| NativeLoadError::Descriptor { path: canonical.clone(), message: format!("module name: {m}") })?;
    if !is_identifier(&name) {
        return Err(NativeLoadError::Descriptor { path: canonical, message: format!("module name '{name}' is not a valid identifier") });
    }
    let target = unsafe { text(desc.target, desc.target_len) }
        .map_err(|m| NativeLoadError::Descriptor { path: canonical.clone(), message: format!("target: {m}") })?;
    if !target_matches(&target) {
        return Err(NativeLoadError::Descriptor {
            path: canonical,
            message: format!(
                "built for target '{target}', this process is {}-{}",
                std::env::consts::ARCH,
                std::env::consts::OS
            ),
        });
    }
    if LOADED.lock().unwrap().iter().any(|(_, m)| m.info.name == name) {
        return Err(NativeLoadError::Descriptor { path: canonical, message: format!("a native module named '{name}' is already loaded") });
    }

    let mut builder = ModuleBuilder { name: name.clone(), functions: Vec::new(), error: None };
    let mut state: *mut c_void = std::ptr::null_mut();
    let init = desc.init.unwrap();
    // SAFETY: module init receives the API table and builder; a panic/unwind here is a module bug.
    let status = unsafe { init(&API, &mut builder as *mut ModuleBuilder as *mut SparModule, &mut state) };
    if status != SPAR_OK {
        return Err(NativeLoadError::Init {
            path: canonical,
            status,
            message: builder.error.take().unwrap_or_else(|| "init returned an error".into()),
        });
    }
    if let Some(m) = builder.error.take() {
        return Err(NativeLoadError::Registration { path: canonical, message: m });
    }
    let info = ModuleInfo {
        name,
        version: (desc.version_major, desc.version_minor, desc.version_patch),
        path: canonical.clone(),
        abi_major: desc.abi_major,
        min_abi_minor: desc.min_abi_minor,
        required_capabilities: desc.required_capabilities,
        optional_capabilities: desc.optional_capabilities,
        target,
        functions: builder.functions.iter().map(|f| f.name.clone()).collect(),
    };
    if debug_enabled() {
        eprintln!(
            "[spar-native] loaded '{}' v{}.{}.{} from {} (abi {}.{}, required caps 0x{:x}, optional 0x{:x}, {} functions: {})",
            info.name,
            info.version.0,
            info.version.1,
            info.version.2,
            info.path.display(),
            info.abi_major,
            info.min_abi_minor,
            info.required_capabilities,
            info.optional_capabilities,
            info.functions.len(),
            info.functions.join(", ")
        );
    }
    let module = Arc::new(LoadedModule {
        info,
        functions: builder.functions,
        state,
        quiesce: desc.quiesce,
        destroy: desc.destroy,
        shut_down: Mutex::new(false),
        _lib: lib,
    });
    LOADED.lock().unwrap().push((canonical, module.clone()));
    Ok(module)
}

impl LoadedModule {
    pub fn info(&self) -> &ModuleInfo {
        &self.info
    }

    /// Registers this module's functions into `registry` under `module::name`.
    pub fn register(&self, registry: &mut NativeRegistry) -> Result<(), SparError> {
        for f in &self.functions {
            let info = f.info.clone();
            if let Some(index) = info.external {
                let params = f.params.iter().map(|(n, t)| (n.as_str(), t.clone())).collect();
                let intrinsic = crate::runtime::NativeIntrinsic::External(index);
                registry.register(NativeFunction::sync_intrinsic(
                    self.info.name.clone(),
                    f.name.clone(),
                    params,
                    f.ret.clone(),
                    false,
                    intrinsic,
                ))?;
                continue;
            }
            if let Some(direct) = info.direct.clone() {
                registry.register(NativeFunction::sync(
                    self.info.name.clone(),
                    f.name.clone(),
                    f.params.iter().map(|(n, t)| (n.as_str(), t.clone())).collect(),
                    f.ret.clone(),
                    false,
                    move |_ctx, args| direct(args),
                ))?;
                continue;
            }
            registry.register(NativeFunction::sync(
                self.info.name.clone(),
                f.name.clone(),
                f.params.iter().map(|(n, t)| (n.as_str(), t.clone())).collect(),
                f.ret.clone(),
                false,
                move |ctx, args| invoke(&info, ctx as *mut RuntimeContext, None, args),
            ))?;
        }
        Ok(())
    }

    /// Runs the module's `quiesce` then `destroy` hooks once. Code stays mapped.
    pub fn shutdown(&self) {
        let mut done = self.shut_down.lock().unwrap();
        if *done {
            return;
        }
        *done = true;
        // SAFETY: hooks were supplied by the module for this state pointer.
        unsafe {
            if let Some(q) = self.quiesce {
                q(self.state);
            }
            if let Some(d) = self.destroy {
                d(self.state);
            }
        }
    }
}

/// Runs shutdown hooks of every loaded module in reverse load order. Call after every runtime
/// context (and therefore every native resource) has been dropped.
pub fn shutdown_all() {
    let modules: Vec<_> = LOADED.lock().unwrap().iter().map(|(_, m)| m.clone()).collect();
    for m in modules.iter().rev() {
        m.shutdown();
    }
}

/// Loads `path` and registers its functions.
pub fn load_into_registry(path: &Path, registry: &mut NativeRegistry) -> Result<ModuleInfo, SparError> {
    let module = load_module(path)?;
    module.register(registry)?;
    Ok(module.info().clone())
}

fn status_name(status: i32) -> &'static str {
    match status {
        SPAR_E_ERROR => "error",
        SPAR_E_TYPE => "type error",
        SPAR_E_RANGE => "range error",
        SPAR_E_OOM => "out of memory",
        SPAR_E_INVALID_ARGUMENT => "invalid argument",
        SPAR_E_WRONG_THREAD => "wrong thread",
        SPAR_E_UNSUPPORTED => "unsupported",
        SPAR_E_CANCELLED => "cancelled",
        SPAR_E_ABI_MISMATCH => "ABI mismatch",
        SPAR_E_INVALID_HANDLE => "invalid or stale handle",
        SPAR_E_BORROW => "borrow violation",
        SPAR_E_UTF8 => "invalid UTF-8",
        SPAR_E_INVALID_STATE => "invalid state",
        SPAR_E_PANIC => "extension panicked",
        _ => "unknown status",
    }
}

fn fail(message: String) -> SparError {
    SparError::EvalError { message, span: Span::dummy() }
}

type HostPtr = Option<*mut (dyn super::CallbackHost + 'static)>;

fn invoke(info: &FnInfo, ctx: *mut RuntimeContext, host: HostPtr, args: &[Value]) -> Result<Value, SparError> {
    invoke_full(info, ctx, host, &Span::dummy(), args).map_err(|fault| fault.into_error())
}

fn invoke_full(
    info: &FnInfo,
    ctx: *mut RuntimeContext,
    host: HostPtr,
    span: &Span,
    args: &[Value],
) -> Result<Value, RuntimeFault> {
    if args.len() != info.argc {
        return Err(fail(format!(
            "native function '{}::{}' expects {} arguments, got {}",
            info.module,
            info.name,
            info.argc,
            args.len()
        ))
        .into());
    }
    let raw = Box::into_raw(CallEnv::acquire(ctx));
    // SAFETY: `raw` is uniquely owned here; the extension only reaches it through the API table,
    // which validates it. It is reclaimed (and the scope closed) below on every path.
    let (status, out, env) = unsafe {
        let env = &mut *raw;
        env.host = host;
        if host.is_some() {
            env.call_span = span.clone();
        }
        for a in args {
            let v = env.borrow_value(a);
            env.argv.push(v);
        }
        let mut out = SparValue::void();
        let envp = env.as_ptr();
        let argv = env.argv.as_ptr();
        let status = (info.invoke)(envp, info.userdata, argv, args.len() as u64, &mut out);
        (status, out, &mut *raw)
    };
    let result = if status == SPAR_OK && info.is_async {
        match env.async_op.take() {
            Some((op, handle)) => {
                super::async_op::mark_running(op);
                Ok(Value::Promise(handle))
            }
            None => Err(fail(format!(
                "async native function '{}::{}' returned without calling async_begin",
                info.module, info.name
            ))
            .into()),
        }
    } else if status == SPAR_OK {
        match env.take_value(&out) {
            Ok(value) => check_ret(info, value).map_err(RuntimeFault::from),
            Err(code) => Err(fail(format!(
                "native function '{}::{}' returned an invalid value ({})",
                info.module,
                info.name,
                status_name(code)
            ))
            .into()),
        }
    } else if let Some(fault) = env.pending_fault.take() {
        Err(fault)
    } else {
        let detail = env.error.take().map(|(_, m)| m);
        Err(fail(match detail {
            Some(m) => format!("{}::{}: {m}", info.module, info.name),
            None => format!("native function '{}::{}' failed: {}", info.module, info.name, status_name(status)),
        })
        .into())
    };
    // SAFETY: reclaim the env allocated above.
    unsafe { Box::from_raw(raw) }.release();
    result
}

/// Runs a `SPAR_FN_CALLS` function with interpreter access (dispatched from `execute_native_intrinsic`).
pub(crate) fn call_external(
    index: u32,
    rt: &mut dyn super::CallbackHost,
    args: &[Value],
    span: &Span,
) -> Result<Value, RuntimeFault> {
    let info = EXTERNALS
        .lock()
        .unwrap()
        .get(index as usize)
        .and_then(|i| i.clone())
        .ok_or_else(|| fail(format!("unknown native module function #{index}")))?;
    let ctx = rt.context_ptr();
    // SAFETY: the trait-object lifetime is erased; the pointer is dropped from the env when the
    // call ends (`end_scope`) and `rt` is not used again until this function returns.
    let host: *mut (dyn super::CallbackHost + 'static) = unsafe { std::mem::transmute(rt as *mut dyn super::CallbackHost) };
    invoke_full(&info, ctx, Some(host), span, args).map_err(|fault| match fault {
        RuntimeFault::Raised(e) => RuntimeFault::Raised(Box::new(attach_span(*e, span))),
        other => other,
    })
}

fn attach_span(error: SparError, span: &Span) -> SparError {
    match error {
        SparError::EvalError { message, span: s } if s.start == 0 && s.end == 0 && s.line == 0 => {
            SparError::EvalError { message, span: span.clone() }
        }
        other => other,
    }
}

#[inline]
fn check_ret(info: &FnInfo, value: Value) -> Result<Value, SparError> {
    let ok = match (info.ret, &value) {
        (RetKind::Any, _) => true,
        (RetKind::Void, Value::Void) => true,
        (RetKind::Int, Value::Int(_)) => true,
        (RetKind::Float, Value::Float(_)) => true,
        (RetKind::Bool, Value::Bool(_)) => true,
        (RetKind::Str, Value::String(_)) => true,
        (RetKind::Float, Value::Int(n)) => return Ok(Value::Float(*n as f64)),
        _ => false,
    };
    if ok {
        Ok(value)
    } else {
        Err(fail(format!(
            "native function '{}::{}' returned {} but is declared to return a different type",
            info.module,
            info.name,
            value.type_name()
        )))
    }
}

/// Convenience used by tests: registered module functions by name.
#[allow(dead_code)]
pub fn loaded_modules() -> HashMap<String, ModuleInfo> {
    LOADED.lock().unwrap().iter().map(|(_, m)| (m.info.name.clone(), m.info.clone())).collect()
}

#[cfg(test)]
pub(crate) mod bench_support {
    use super::*;
    unsafe extern "C" fn noop(_e: *mut SparEnv, _u: *mut c_void, _a: *const SparValue, _n: u64, o: *mut SparValue) -> spar_status_t {
        *o = SparValue::void();
        SPAR_OK
    }
    pub(crate) fn info() -> FnInfo {
        FnInfo { module: "m".into(), name: "f".into(), invoke: noop, userdata: std::ptr::null_mut(), ret: RetKind::Void, argc: 0, external: None, is_async: false, direct: None }
    }
    pub(crate) fn call(info: &FnInfo, ctx: &mut RuntimeContext) -> Result<Value, SparError> {
        invoke(info, ctx as *mut RuntimeContext, None, &[])
    }
    pub(crate) fn step1(info: &FnInfo, ctx: &mut RuntimeContext) -> Result<Value, SparError> {
        let raw = Box::into_raw(CallEnv::acquire(ctx as *mut RuntimeContext));
        let (status, out, env) = unsafe {
            let env = &mut *raw;
            env.host = None;
            let mut out = SparValue::void();
            let envp = env.as_ptr();
            let argv = env.argv.as_ptr();
            let status = (info.invoke)(envp, info.userdata, argv, 0, &mut out);
            (status, out, &mut *raw)
        };
        let r = if status == SPAR_OK { env.take_value(&out).map_err(|_| fail("x".into())) } else { Err(fail("y".into())) };
        unsafe { Box::from_raw(raw) }.release();
        r
    }
    pub(crate) fn v_a(info: &FnInfo, ctx: &mut RuntimeContext) -> Result<Value, SparError> {
        // no host assignment, take_value result ignored
        let raw = Box::into_raw(CallEnv::acquire(ctx as *mut RuntimeContext));
        let status = unsafe {
            let env = &mut *raw;
            let mut out = SparValue::void();
            let envp = env.as_ptr();
            let argv = env.argv.as_ptr();
            (info.invoke)(envp, info.userdata, argv, 0, &mut out)
        };
        unsafe { Box::from_raw(raw) }.release();
        if status == SPAR_OK { Ok(Value::Void) } else { Err(fail("y".into())) }
    }
    pub(crate) fn v_b(info: &FnInfo, ctx: &mut RuntimeContext) -> Result<Value, SparError> {
        // as v_a but with take_value
        let raw = Box::into_raw(CallEnv::acquire(ctx as *mut RuntimeContext));
        let (status, out) = unsafe {
            let env = &mut *raw;
            let mut out = SparValue::void();
            let envp = env.as_ptr();
            let argv = env.argv.as_ptr();
            ((info.invoke)(envp, info.userdata, argv, 0, &mut out), out)
        };
        let v = if status == SPAR_OK { unsafe { (*raw).take_value(&out) }.ok() } else { None };
        unsafe { Box::from_raw(raw) }.release();
        match v { Some(v) => Ok(v), None => Err(fail("y".into())) }
    }
    pub(crate) fn step2(info: &FnInfo, ctx: &mut RuntimeContext) -> Result<Value, SparError> {
        let raw = Box::into_raw(CallEnv::acquire(ctx as *mut RuntimeContext));
        let (status, out, env) = unsafe {
            let env = &mut *raw;
            env.host = None;
            let mut out = SparValue::void();
            let envp = env.as_ptr();
            let argv = env.argv.as_ptr();
            let status = (info.invoke)(envp, info.userdata, argv, 0, &mut out);
            (status, out, &mut *raw)
        };
        let r = if status == SPAR_OK { match env.take_value(&out) { Ok(v) => check_ret(info, v), Err(_) => Err(fail("x".into())) } } else { Err(fail("y".into())) };
        unsafe { Box::from_raw(raw) }.release();
        r
    }
    pub(crate) fn call_full(info: &FnInfo, ctx: &mut RuntimeContext) -> Result<Value, RuntimeFault> {
        invoke_full(info, ctx as *mut RuntimeContext, None, &Span::dummy(), &[])
    }
}
