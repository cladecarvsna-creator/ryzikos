/* The bridge between QuickJS and the EverOS kernel (kernel/src/js).
 *
 * Rust never touches a JSValue: it evaluates source text through these
 * functions and scripts reach the kernel through one native function,
 * __native(op, ...args), whose arguments arrive in Rust as strings. */

#include <stdlib.h>
#include <string.h>
#include "quickjs.h"

/* In Rust. */
int everos_js_native(const char *op, size_t op_len, int argc, const char **argv, const size_t *lens,
                     char **out, size_t *out_len);
int everos_js_interrupt(void);
char *everos_js_resolve_module(const char *base, const char *name);
char *everos_js_load_module(const char *name, size_t *len);

enum {
    RESULT_UNDEFINED = 0,
    RESULT_STRING = 1,
    RESULT_INT = 2,
    RESULT_JSON = 3,
    RESULT_TRUE = 4,
    RESULT_FALSE = 5,
    RESULT_NULL = 6,
    RESULT_ERROR = 7,
};

#define MAX_ARGS 8

static JSValue js_native(JSContext *ctx, JSValueConst this_val, int argc, JSValueConst *argv)
{
    const char *strs[MAX_ARGS];
    size_t lens[MAX_ARGS];
    int n = argc < MAX_ARGS ? argc : MAX_ARGS;
    int i;
    (void)this_val;
    if (n < 1)
        return JS_UNDEFINED;
    for (i = 0; i < n; i++) {
        if (JS_IsNull(argv[i]) || JS_IsUndefined(argv[i])) {
            strs[i] = NULL;
            lens[i] = 0;
            continue;
        }
        strs[i] = JS_ToCStringLen(ctx, &lens[i], argv[i]);
        if (!strs[i]) {
            while (--i >= 0)
                JS_FreeCString(ctx, strs[i]);
            return JS_EXCEPTION;
        }
    }
    char *out = NULL;
    size_t out_len = 0;
    int kind = everos_js_native(strs[0] ? strs[0] : "", lens[0], n - 1, strs + 1, lens + 1, &out, &out_len);
    for (i = 0; i < n; i++)
        if (strs[i])
            JS_FreeCString(ctx, strs[i]);

    JSValue ret;
    switch (kind) {
    case RESULT_STRING:
        ret = JS_NewStringLen(ctx, out ? out : "", out_len);
        break;
    case RESULT_INT: {
        long long v = 0;
        int neg = 0;
        size_t k = 0;
        if (out_len && out[0] == '-')
            neg = 1, k = 1;
        for (; k < out_len; k++)
            v = v * 10 + (out[k] - '0');
        ret = JS_NewInt64(ctx, neg ? -v : v);
        break;
    }
    case RESULT_JSON: {
        /* JS_ParseJSON wants a terminated buffer */
        char *buf = malloc(out_len + 1);
        memcpy(buf, out, out_len);
        buf[out_len] = 0;
        ret = JS_ParseJSON(ctx, buf, out_len, "<native>");
        free(buf);
        break;
    }
    case RESULT_TRUE:
        ret = JS_TRUE;
        break;
    case RESULT_FALSE:
        ret = JS_FALSE;
        break;
    case RESULT_NULL:
        ret = JS_NULL;
        break;
    case RESULT_ERROR:
        ret = JS_ThrowTypeError(ctx, "%.*s", (int)out_len, out ? out : "");
        break;
    default:
        ret = JS_UNDEFINED;
    }
    free(out);
    return ret;
}

static int interrupt_handler(JSRuntime *rt, void *opaque)
{
    (void)rt;
    (void)opaque;
    return everos_js_interrupt();
}

/* ES modules: Rust resolves the address and downloads the source. */
static char *module_normalize(JSContext *ctx, const char *base, const char *name, void *opaque)
{
    (void)opaque;
    char *r = everos_js_resolve_module(base, name);
    if (!r)
        return js_strdup(ctx, name);
    char *copy = js_strdup(ctx, r);
    free(r);
    return copy;
}

static JSModuleDef *module_loader(JSContext *ctx, const char *name, void *opaque)
{
    size_t len = 0;
    (void)opaque;
    char *src = everos_js_load_module(name, &len);
    if (!src) {
        JS_ThrowReferenceError(ctx, "could not load module '%s'", name);
        return NULL;
    }
    JSValue f = JS_Eval(ctx, src, len, name, JS_EVAL_TYPE_MODULE | JS_EVAL_FLAG_COMPILE_ONLY);
    free(src);
    if (JS_IsException(f))
        return NULL;
    JSModuleDef *m = JS_VALUE_GET_PTR(f);
    JS_FreeValue(ctx, f);
    return m;
}

JSContext *ejs_new(size_t memory_limit, size_t stack_size)
{
    JSRuntime *rt = JS_NewRuntime();
    if (!rt)
        return NULL;
    JS_SetMemoryLimit(rt, memory_limit);
    JS_SetMaxStackSize(rt, stack_size);
    JS_SetInterruptHandler(rt, interrupt_handler, NULL);
    JS_SetModuleLoaderFunc(rt, module_normalize, module_loader, NULL);
    JSContext *ctx = JS_NewContext(rt);
    if (!ctx) {
        JS_FreeRuntime(rt);
        return NULL;
    }
    JSValue global = JS_GetGlobalObject(ctx);
    JS_SetPropertyStr(ctx, global, "__native", JS_NewCFunction(ctx, js_native, "__native", 1));
    JS_FreeValue(ctx, global);
    return ctx;
}

void ejs_free(JSContext *ctx)
{
    JSRuntime *rt = JS_GetRuntime(ctx);
    JS_FreeContext(ctx);
    JS_FreeRuntime(rt);
}

/* A malloc'ed, terminated copy of a value's text (or of the pending
 * exception with its stack trace). */
static char *describe(JSContext *ctx, JSValueConst v, size_t *len)
{
    size_t n = 0;
    const char *s = JS_ToCStringLen(ctx, &n, v);
    char *copy;
    if (!s) {
        JS_FreeValue(ctx, JS_GetException(ctx));
        s = NULL;
        n = 0;
    }
    int is_error = JS_IsError(ctx, v);
    JSValue stack = is_error ? JS_GetPropertyStr(ctx, v, "stack") : JS_UNDEFINED;
    size_t sn = 0;
    const char *st = JS_IsString(stack) ? JS_ToCStringLen(ctx, &sn, stack) : NULL;
    copy = malloc(n + sn + 2);
    if (s)
        memcpy(copy, s, n);
    if (st) {
        copy[n] = '\n';
        memcpy(copy + n + 1, st, sn);
        n += sn + 1;
        JS_FreeCString(ctx, st);
    }
    copy[n] = 0;
    if (s)
        JS_FreeCString(ctx, s);
    JS_FreeValue(ctx, stack);
    *len = n;
    return copy;
}

/* Run pending promise jobs. */
void ejs_run_jobs(JSContext *ctx)
{
    JSContext *job_ctx;
    int budget = 100000;
    while (budget-- > 0 && JS_ExecutePendingJob(JS_GetRuntime(ctx), &job_ctx) > 0)
        ;
}

/* Evaluate a script (as global code, or as a module when module is set).
 * Returns 0 on success with the result's text in *out (when out is not
 * NULL), or -1 with the error in *out. *out is malloc'ed; the caller
 * frees it with free(). */
int ejs_eval(JSContext *ctx, const char *src, size_t len, const char *filename, int module, char **out,
             size_t *out_len)
{
    /* the stack may differ from the last call (pages load on fibers) */
    JS_UpdateStackTop(JS_GetRuntime(ctx));
    /* QuickJS wants the source terminated */
    char *buf = malloc(len + 1);
    memcpy(buf, src, len);
    buf[len] = 0;
    JSValue v = JS_Eval(ctx, buf, len, filename, module ? JS_EVAL_TYPE_MODULE : JS_EVAL_TYPE_GLOBAL);
    free(buf);
    int ok = !JS_IsException(v);
    if (!ok)
        v = JS_GetException(ctx);
    ejs_run_jobs(ctx);
    if (ok && module && JS_PromiseState(ctx, v) == JS_PROMISE_REJECTED) {
        JSValue reason = JS_PromiseResult(ctx, v);
        JS_FreeValue(ctx, v);
        v = reason;
        ok = 0;
    }
    if (out)
        *out = describe(ctx, v, out_len);
    JS_FreeValue(ctx, v);
    return ok ? 0 : -1;
}

size_t ejs_memory_used(JSContext *ctx)
{
    JSMemoryUsage m;
    JS_ComputeMemoryUsage(JS_GetRuntime(ctx), &m);
    return (size_t)m.malloc_size;
}
