#include <emacs-module.h>
#import <Cocoa/Cocoa.h>
#import <objc/runtime.h>
#include <string.h>
int plugin_is_GPL_compatible;
static emacs_value Fdump(emacs_env *env, ptrdiff_t n, emacs_value *a, void *d) {
  NSMutableString *s = [NSMutableString string];
  for (NSWindow *w in [NSApp windows]) {
    [s appendFormat:@"window '%@' #%ld\n", [w title], (long)[w windowNumber]];
    for (NSView *v in [[w contentView] subviews]) {
      if (![NSStringFromClass([v class]) isEqualToString:@"EmacsView"]) continue;
      unsigned int c = 0; Ivar *iv = class_copyIvarList([v class], &c);
      [s appendFormat:@"  EmacsView ivars (%u):", c];
      for (unsigned i = 0; i < c; i++) {
        const char *nm = ivar_getName(iv[i]); const char *tp = ivar_getTypeEncoding(iv[i]);
        ptrdiff_t off = ivar_getOffset(iv[i]);
        // 型の符号化は長い（struct frame の中身が全部出る）ので先頭だけ
        char t[24]; strncpy(t, tp, sizeof t - 1); t[sizeof t - 1] = 0;
        [s appendFormat:@"\n    %s : %s%s", nm, t, strlen(tp) > sizeof t - 1 ? "..." : ""];
        if (tp[0] == 'i' || tp[0] == 'l' || tp[0] == 'q')
          [s appendFormat:@" = %d", *(int *)((char *)(__bridge void *)v + off)];
        if (tp[0] == '^')
          [s appendFormat:@" = %p", *(void **)((char *)(__bridge void *)v + off)];
      }
      free(iv);
      [s appendString:@"\n"];
    }
  }
  const char *c = [s UTF8String];
  return env->make_string(env, c, strlen(c));
}
int emacs_module_init(struct emacs_runtime *ert) {
  emacs_env *env = ert->get_environment(ert);
  emacs_value args[] = { env->intern(env, "ivar-dump"), env->make_function(env, 0, 0, Fdump, NULL, NULL) };
  env->funcall(env, env->intern(env, "defalias"), 2, args);
  return 0;
}
