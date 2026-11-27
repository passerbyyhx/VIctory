struct guid { unsigned long a; unsigned short b, c; unsigned char tail[8]; };
__declspec(dllimport) long QueryApiImpl(const struct guid *, const struct guid *, void **);
__declspec(dllimport) const char *GetCommandLineA(void);
static int has(const char *s, const char *word) {
    for (; *s; ++s) {
        const char *a = s, *b = word;
        while (*b && *a == *b) { ++a; ++b; }
        if (!*b) return 1;
    }
    return 0;
}
int entry(void) {
    const char *cmd = GetCommandLineA();
#ifdef CHILD
    return has(cmd, "child-cookie") && !has(cmd, "first-instance") ? 73 : 74;
#else
    if (!has(cmd, "first-instance") || has(cmd, "child-cookie")) return 41;
    static const struct guid provider = {0x973a344e,0x24bf,0x4d0f,{0x84,0x57,0x56,0xc5,0x34,0x89,0x2b,0x29}};
    static const struct guid iid = {0x2549f142,0x6419,0x4a06,{0x97,0xb5,0x93,0x1a,0xab,0x7c,0x2f,0x34}};
    void *obj = 0;
    if (QueryApiImpl(&provider,&iid,&obj) || !obj) return 42;
    typedef void (*launch_fn)(void *,const char *,const char *,void *);
    ((launch_fn)(*(void ***)obj)[4])(obj,"Other.exe","child-cookie",0);
    return 43;
#endif
}
