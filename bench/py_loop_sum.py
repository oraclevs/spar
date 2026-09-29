def sum_to(n: int) -> int:
    total = 0
    for i in range(n):
        total = total + i * 3 - (i // 7)
    return total
print(sum_to(100000000))
