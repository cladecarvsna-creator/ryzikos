/* The small part of the C library QuickJS needs, for the EverOS kernel.
 *
 * Memory, time and assertion failures are implemented in Rust
 * (kernel/src/js/libc.rs). The kernel's Rust code is built without SSE,
 * so doubles cannot cross into it directly: the maths functions pass
 * their arguments and results as raw 64-bit patterns instead. */

#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <fenv.h>
#include <errno.h>

int errno;

/* ---- strings ------------------------------------------------------------- */

void *memchr(const void *s, int c, size_t n)
{
    const unsigned char *p = s;
    for (; n; n--, p++)
        if (*p == (unsigned char)c)
            return (void *)p;
    return NULL;
}

char *strchr(const char *s, int c)
{
    for (;; s++) {
        if (*s == (char)c)
            return (char *)s;
        if (!*s)
            return NULL;
    }
}

char *strrchr(const char *s, int c)
{
    const char *last = NULL;
    for (;; s++) {
        if (*s == (char)c)
            last = s;
        if (!*s)
            return (char *)last;
    }
}

int strcmp(const char *a, const char *b)
{
    while (*a && *a == *b)
        a++, b++;
    return (unsigned char)*a - (unsigned char)*b;
}

int strncmp(const char *a, const char *b, size_t n)
{
    for (; n; n--, a++, b++) {
        if (*a != *b || !*a)
            return (unsigned char)*a - (unsigned char)*b;
    }
    return 0;
}

int abs(int x)
{
    return x < 0 ? -x : x;
}

/* ---- formatted output ---------------------------------------------------- */

struct out {
    char *buf;
    size_t cap, len;
};

static void put(struct out *o, char c)
{
    if (o->len + 1 < o->cap)
        o->buf[o->len] = c;
    o->len++;
}

static void put_padded(struct out *o, const char *s, size_t n, int width, int left, char pad)
{
    int fill = width > (int)n ? width - (int)n : 0;
    if (!left)
        while (fill-- > 0)
            put(o, pad);
    while (n--)
        put(o, *s++);
    if (left)
        while (fill-- > 0)
            put(o, ' ');
}

int vsnprintf(char *buf, size_t cap, const char *fmt, va_list ap)
{
    struct out o = { buf, cap, 0 };
    char tmp[72];
    for (; *fmt; fmt++) {
        if (*fmt != '%') {
            put(&o, *fmt);
            continue;
        }
        fmt++;
        int left = 0, plus = 0, space = 0, alt = 0;
        char pad = ' ';
        for (;; fmt++) {
            if (*fmt == '-') left = 1;
            else if (*fmt == '0') pad = '0';
            else if (*fmt == '+') plus = 1;
            else if (*fmt == ' ') space = 1;
            else if (*fmt == '#') alt = 1;
            else break;
        }
        int width = 0, prec = -1;
        if (*fmt == '*') {
            width = va_arg(ap, int);
            fmt++;
        } else {
            while (*fmt >= '0' && *fmt <= '9')
                width = width * 10 + (*fmt++ - '0');
        }
        if (*fmt == '.') {
            fmt++;
            prec = 0;
            if (*fmt == '*') {
                prec = va_arg(ap, int);
                fmt++;
            } else {
                while (*fmt >= '0' && *fmt <= '9')
                    prec = prec * 10 + (*fmt++ - '0');
            }
        }
        int size = 0; /* 0 int, 1 long, 2 long long, 3 size_t */
        for (;; fmt++) {
            if (*fmt == 'l') size++;
            else if (*fmt == 'z' || *fmt == 'j' || *fmt == 't') size = 2;
            else if (*fmt == 'h') ;
            else break;
        }
        char c = *fmt;
        if (!c)
            break;
        switch (c) {
        case 'd':
        case 'i':
        case 'u':
        case 'x':
        case 'X':
        case 'o':
        case 'p': {
            unsigned long long v;
            int neg = 0;
            if (c == 'p') {
                v = (uintptr_t)va_arg(ap, void *);
                alt = 1;
            } else if (c == 'd' || c == 'i') {
                long long s = size >= 2 ? va_arg(ap, long long) : size == 1 ? va_arg(ap, long) : va_arg(ap, int);
                neg = s < 0;
                v = neg ? -(unsigned long long)s : (unsigned long long)s;
            } else {
                v = size >= 2 ? va_arg(ap, unsigned long long) : size == 1 ? va_arg(ap, unsigned long) : va_arg(ap, unsigned int);
            }
            unsigned base = (c == 'x' || c == 'X' || c == 'p') ? 16 : c == 'o' ? 8 : 10;
            const char *digits = c == 'X' ? "0123456789ABCDEF" : "0123456789abcdef";
            char *end = tmp + sizeof(tmp), *p = end;
            do {
                *--p = digits[v % base];
                v /= base;
            } while (v);
            while (prec > 0 && end - p < prec)
                *--p = '0';
            if (alt && base == 16)
                *--p = 'x', *--p = '0';
            if (neg)
                *--p = '-';
            else if (plus)
                *--p = '+';
            else if (space)
                *--p = ' ';
            put_padded(&o, p, end - p, width, left, left ? ' ' : pad);
            break;
        }
        case 'c':
            tmp[0] = (char)va_arg(ap, int);
            put_padded(&o, tmp, 1, width, left, ' ');
            break;
        case 's': {
            const char *s = va_arg(ap, const char *);
            if (!s)
                s = "(null)";
            size_t n = prec >= 0 ? strnlen(s, prec) : strlen(s);
            put_padded(&o, s, n, width, left, ' ');
            break;
        }
        case 'f':
        case 'g':
        case 'e':
        case 'F':
        case 'G':
        case 'E':
            /* only QuickJS's debug dumps print floats */
            (void)va_arg(ap, double);
            put_padded(&o, "?", 1, width, left, ' ');
            break;
        case '%':
            put(&o, '%');
            break;
        default:
            put(&o, '%');
            put(&o, c);
        }
    }
    if (cap)
        buf[o.len < cap ? o.len : cap - 1] = 0;
    return (int)o.len;
}

size_t strnlen(const char *s, size_t n)
{
    size_t i = 0;
    while (i < n && s[i])
        i++;
    return i;
}

int snprintf(char *buf, size_t n, const char *fmt, ...)
{
    va_list ap;
    va_start(ap, fmt);
    int r = vsnprintf(buf, n, fmt, ap);
    va_end(ap);
    return r;
}

void everos_log(const char *s, size_t n);

int vfprintf(FILE *f, const char *fmt, va_list ap)
{
    char buf[512];
    (void)f;
    int n = vsnprintf(buf, sizeof(buf), fmt, ap);
    everos_log(buf, n < (int)sizeof(buf) ? (size_t)n : sizeof(buf) - 1);
    return n;
}

int fprintf(FILE *f, const char *fmt, ...)
{
    va_list ap;
    va_start(ap, fmt);
    int r = vfprintf(f, fmt, ap);
    va_end(ap);
    return r;
}

int printf(const char *fmt, ...)
{
    va_list ap;
    va_start(ap, fmt);
    int r = vfprintf(NULL, fmt, ap);
    va_end(ap);
    return r;
}

int fputc(int c, FILE *f)
{
    char ch = (char)c;
    (void)f;
    everos_log(&ch, 1);
    return c;
}

int putchar(int c)
{
    return fputc(c, NULL);
}

int fputs(const char *s, FILE *f)
{
    (void)f;
    everos_log(s, strlen(s));
    return 0;
}

size_t fwrite(const void *p, size_t size, size_t n, FILE *f)
{
    (void)f;
    everos_log(p, size * n);
    return n;
}

FILE *stdout, *stderr, *stdin;

/* ---- maths --------------------------------------------------------------- */

/* Implemented in Rust with the libm crate, on raw bit patterns. */
uint64_t everos_math1(int op, uint64_t x);
uint64_t everos_math2(int op, uint64_t x, uint64_t y);

static inline uint64_t bits(double x)
{
    union { double d; uint64_t u; } v = { .d = x };
    return v.u;
}

static inline double from_bits(uint64_t u)
{
    union { double d; uint64_t u; } v = { .u = u };
    return v.d;
}

#define MATH1(name, op) \
    double name(double x) { return from_bits(everos_math1(op, bits(x))); }
#define MATH2(name, op) \
    double name(double x, double y) { return from_bits(everos_math2(op, bits(x), bits(y))); }

MATH1(floor, 0)
MATH1(ceil, 1)
MATH1(trunc, 2)
MATH1(round, 3)
MATH1(sin, 4)
MATH1(cos, 5)
MATH1(tan, 6)
MATH1(asin, 7)
MATH1(acos, 8)
MATH1(atan, 9)
MATH1(sinh, 10)
MATH1(cosh, 11)
MATH1(tanh, 12)
MATH1(asinh, 13)
MATH1(acosh, 14)
MATH1(atanh, 15)
MATH1(exp, 16)
MATH1(expm1, 17)
MATH1(log, 18)
MATH1(log1p, 19)
MATH1(log2, 20)
MATH1(log10, 21)
MATH1(cbrt, 22)
MATH1(rint, 23)
MATH2(pow, 0)
MATH2(atan2, 1)
MATH2(fmod, 2)
MATH2(hypot, 3)
MATH2(fmin, 4)
MATH2(fmax, 5)

double nearbyint(double x)
{
    return rint(x);
}

long lrint(double x)
{
    return (long)rint(x);
}

int fesetround(int mode)
{
    (void)mode;
    return 0;
}

int fegetround(void)
{
    return FE_TONEAREST;
}
