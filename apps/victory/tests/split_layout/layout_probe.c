/* A self-contained title with one imported title DLL and one file in G:. */
__declspec(dllimport) unsigned int marker(void);
__declspec(dllimport) void * __stdcall CreateFileW(const unsigned short *, unsigned long,
    unsigned long, void *, unsigned long, unsigned long, void *);
__declspec(dllimport) int __stdcall ReadFile(void *, void *, unsigned long, unsigned long *, void *);
__declspec(dllimport) int __stdcall CloseHandle(void *);

unsigned int entry(void) {
    unsigned char value = 0;
    unsigned long count = 0;
    void *file = CreateFileW(L"G:\\fixture.bin", 0x80000000, 1, 0, 3, 0x80, 0);
    if (file == (void *)-1) return 240;
    int ok = ReadFile(file, &value, 1, &count, 0);
    CloseHandle(file);
    if (!ok || count != 1) return 241;
    return 16 * marker() + value;
}
