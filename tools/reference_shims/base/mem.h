#pragma once
#include <cstddef>
#include <cstring>
inline void mem_zero(void *data, size_t size) { std::memset(data, 0, size); }
