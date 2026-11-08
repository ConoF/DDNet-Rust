#pragma once
#include <cstdlib>
#define dbg_assert(expr, ...) do { if(!(expr)) std::abort(); } while(0)
