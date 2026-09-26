#ifndef _EVEROS_MATH_H
#define _EVEROS_MATH_H
#define NAN __builtin_nan("")
#define INFINITY __builtin_inf()
#define HUGE_VAL __builtin_huge_val()
#define M_PI 3.14159265358979323846
#define isnan(x) __builtin_isnan(x)
#define isinf(x) __builtin_isinf(x)
#define isfinite(x) __builtin_isfinite(x)
#define signbit(x) __builtin_signbit(x)
#define fpclassify(x) __builtin_fpclassify(FP_NAN, FP_INFINITE, FP_NORMAL, FP_SUBNORMAL, FP_ZERO, x)
#define FP_NAN 0
#define FP_INFINITE 1
#define FP_ZERO 2
#define FP_SUBNORMAL 3
#define FP_NORMAL 4
static inline double fabs(double x) { return __builtin_fabs(x); }
static inline double sqrt(double x) { return __builtin_sqrt(x); }
static inline float sqrtf(float x) { return __builtin_sqrtf(x); }
static inline double copysign(double x, double y) { return __builtin_copysign(x, y); }
double floor(double), ceil(double), trunc(double), round(double), rint(double), nearbyint(double);
long lrint(double);
double fmod(double, double), fmin(double, double), fmax(double, double);
double sin(double), cos(double), tan(double), asin(double), acos(double), atan(double), atan2(double, double);
double sinh(double), cosh(double), tanh(double), asinh(double), acosh(double), atanh(double);
double exp(double), expm1(double), log(double), log1p(double), log2(double), log10(double);
double pow(double, double), cbrt(double), hypot(double, double);
double frexp(double, int *), ldexp(double, int), scalbn(double, int), modf(double, double *);
#endif
