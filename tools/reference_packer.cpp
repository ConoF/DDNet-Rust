// Reference-only test: the Rust client does not link or load C++.
#include "../upstream/ddnet/src/engine/shared/compression.h"

#include <climits>
#include <cstdio>

static_assert(sizeof(int) == 4, "DDNet wire integers are 32-bit");

int main()
{
    const int aValues[] = {
        INT_MIN, -65536, -65, -64, -2, -1, 0, 1, 63, 64, 255, 65535, INT_MAX,
    };

    for(const int Value : aValues)
    {
        unsigned char aBuffer[5] = {0};
        const unsigned char *pEnd = CVariableInt::Pack(aBuffer, Value, sizeof(aBuffer));
        if(!pEnd)
            return 1;
        std::printf("%d ", Value);
        for(const unsigned char *p = aBuffer; p != pEnd; ++p)
            std::printf("%02x", static_cast<unsigned>(*p));
        std::putchar('\n');
    }
    return 0;
}
