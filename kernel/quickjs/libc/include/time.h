#ifndef _EVEROS_TIME_H
#define _EVEROS_TIME_H
#include <stddef.h>
typedef long long time_t;
struct tm {
    int tm_sec, tm_min, tm_hour, tm_mday, tm_mon, tm_year, tm_wday, tm_yday, tm_isdst;
    long tm_gmtoff;
    const char *tm_zone;
};
struct timespec { time_t tv_sec; long tv_nsec; };
time_t time(time_t *t);
struct tm *localtime_r(const time_t *t, struct tm *out);
struct tm *gmtime_r(const time_t *t, struct tm *out);
#define CLOCK_REALTIME 0
#define CLOCK_MONOTONIC 1
typedef int clockid_t;
int clock_gettime(clockid_t id, struct timespec *ts);
#endif
