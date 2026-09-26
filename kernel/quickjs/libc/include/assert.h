#ifdef NDEBUG
#define assert(x) ((void)0)
#else
_Noreturn void __everos_assert(const char *expr, const char *file, int line);
#define assert(x) ((x) ? (void)0 : __everos_assert(#x, __FILE__, __LINE__))
#endif
