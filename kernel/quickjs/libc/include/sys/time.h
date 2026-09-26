#include <time.h>
struct timeval { time_t tv_sec; long tv_usec; };
struct timezone { int tz_minuteswest, tz_dsttime; };
int gettimeofday(struct timeval *tv, void *tz);
