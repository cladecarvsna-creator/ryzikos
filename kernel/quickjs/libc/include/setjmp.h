/* dtoa.c includes this but never uses it. */
typedef unsigned long jmp_buf[8];
int setjmp(jmp_buf env) __attribute__((returns_twice));
_Noreturn void longjmp(jmp_buf env, int val);
