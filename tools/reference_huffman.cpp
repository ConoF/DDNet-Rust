// Standalone harness using DDNet 20.2.1's original huffman.cpp unmodified.
#include <engine/shared/huffman.h>
#include <cstdio>
#include <cstdlib>
#include <vector>

int main()
{
    CHuffman codec;
    codec.Init();
    std::vector<std::vector<unsigned char>> inputs;
    inputs.emplace_back();
    inputs.push_back({0});
    const char text[] = "DDNet packet";
    inputs.emplace_back(text, text + sizeof(text) - 1);
    std::vector<unsigned char> all_bytes;
    for(int i = 0; i < 256; ++i)
        all_bytes.push_back(static_cast<unsigned char>(i));
    inputs.push_back(all_bytes);
    inputs.push_back(std::vector<unsigned char>(512, 0));
    std::vector<unsigned char> incrementing;
    for(int i = 0; i < 512; ++i)
        incrementing.push_back(static_cast<unsigned char>(i & 255));
    inputs.push_back(incrementing);

    for(size_t index = 0; index < inputs.size(); ++index)
    {
        unsigned char buffer[10000] = {};
        const int size = codec.Compress(inputs[index].data(),
            static_cast<int>(inputs[index].size()), buffer, sizeof(buffer));
        if(size < 0)
            return 2;
        std::printf("%zu ", index);
        for(int i = 0; i < size; ++i)
            std::printf("%02x", static_cast<unsigned>(buffer[i]));
        std::putchar('\n');
    }
}
