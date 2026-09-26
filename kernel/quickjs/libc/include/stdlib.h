#ifndef _EVEROS_STDLIB_H
#define _EVEROS_STDLIB_H
#include <stddef.h>
void *malloc(size_t size);
void *calloc(size_t n, size_t size);
void *realloc(void *ptr, size_t size);
void free(void *ptr);
_Noreturn void abort(void);
_Noreturn void exit(int status);
int abs(int x);
long labs(long x);
long long llabs(long long x);
long strtol(const char *s, char **end, int base);
unsigned long strtoul(const char *s, char **end, int base);
long long strtoll(const char *s, char **end, int base);
unsigned long long strtoull(const char *s, char **end, int base);
double strtod(const char *s, char **end);
double atof(const char *s);
int atoi(const char *s);
char *getenv(const char *name);
void qsort(void *base, size_t n, size_t size, int (*cmp)(const void *, const void *));
int rand(void);
#define RAND_MAX 0x7fffffff
#define EXIT_SUCCESS 0
#define EXIT_FAILURE 1

#define alloca __builtin_alloca
#endif
