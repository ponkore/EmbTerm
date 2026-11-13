// EmbTerm macOS port spike: Emacs の NS フレームに子 NSView を重ねられるか
#include <emacs-module.h>
#import <Cocoa/Cocoa.h>
#import <QuartzCore/QuartzCore.h>
#include <string.h>
#include <stdio.h>

int plugin_is_GPL_compatible;

static NSMutableString *g_log;
static NSView *g_child;

static void LOG(NSString *fmt, ...) {
  va_list ap; va_start(ap, fmt);
  NSString *s = [[NSString alloc] initWithFormat:fmt arguments:ap];
  va_end(ap);
  [g_log appendFormat:@"%@\n", s];
  fprintf(stderr, "SPIKE: %s\n", [s UTF8String]);
}

static emacs_value Qnil(emacs_env *env) { return env->intern(env, "nil"); }
static emacs_value Qt(emacs_env *env) { return env->intern(env, "t"); }

static NSWindow *window_for_title(NSString *title) {
  for (NSWindow *x in [NSApp windows]) {
    LOG(@"  window #%ld %@ title=%@ visible=%d contentView=%@ subviews=%@",
        (long)[x windowNumber], [x class], [x title], (int)[x isVisible],
        [[x contentView] class], [[x contentView] subviews]);
    if ([[x title] isEqualToString:title]) return x;
  }
  return nil;
}

// (spike-attach TITLE X Y W H) -> t/nil  タイトルで NSWindow を引き、EmacsView の中に子 NSView を重ねる
static emacs_value Fattach(emacs_env *env, ptrdiff_t n, emacs_value *a, void *d) {
  ptrdiff_t tl = 0; env->copy_string_contents(env, a[0], NULL, &tl);
  char *tbuf = malloc(tl); env->copy_string_contents(env, a[0], tbuf, &tl);
  NSString *title = [NSString stringWithUTF8String:tbuf]; free(tbuf);
  CGFloat x = env->extract_integer(env, a[1]), y = env->extract_integer(env, a[2]);
  CGFloat w = env->extract_integer(env, a[3]), h = env->extract_integer(env, a[4]);
  LOG(@"attach: main thread = %d", (int)[NSThread isMainThread]);
  NSWindow *win = window_for_title(title);
  if (!win) return Qnil(env);
  NSView *content = [win contentView];
  for (NSView *sv in [content subviews])
    LOG(@"  subview %@ frame=%@ flipped=%d layer=%@", [sv class], NSStringFromRect([sv frame]), (int)[sv isFlipped], [sv layer]);
  LOG(@"contentView %@ flipped=%d layer=%@ wantsLayer=%d frame=%@ backingScale=%g",
      [content class], (int)[content isFlipped], [content layer], (int)[content wantsLayer],
      NSStringFromRect([content frame]), [win backingScaleFactor]);
  LOG(@"contentView subviews = %lu: %@", (unsigned long)[[content subviews] count], [content subviews]);

  NSRect r = [content isFlipped] ? NSMakeRect(x, y, w, h)
                                 : NSMakeRect(x, NSHeight([content frame]) - y - h, w, h);
  NSView *v = [[NSView alloc] initWithFrame:r];
  [v setWantsLayer:YES];
  CALayer *l = [v layer];
  [l setBackgroundColor:CGColorCreateGenericRGB(0.9, 0.2, 0.2, 1.0)];
  [l setContentsScale:[win backingScaleFactor]];
  // EmacsView（フレーム本体のビュー）の中に入れる
  NSView *host = content;
  for (NSView *sv in [content subviews])
    if ([NSStringFromClass([sv class]) isEqualToString:@"EmacsView"]) host = sv;
  r = [host isFlipped] ? NSMakeRect(x, y, w, h) : NSMakeRect(x, NSHeight([host frame]) - y - h, w, h);
  [v setFrame:r];
  LOG(@"host view = %@ flipped=%d frame=%@", [host class], (int)[host isFlipped], NSStringFromRect([host frame]));
  [host addSubview:v positioned:NSWindowAbove relativeTo:nil];
  g_child = v;
  LOG(@"added child view: frame=%@ superview=%@ window=%@ hidden=%d",
      NSStringFromRect([v frame]), [[v superview] class], [[v window] class], (int)[v isHiddenOrHasHiddenAncestor]);
  LOG(@"after add, contentView subviews = %lu", (unsigned long)[[content subviews] count]);
  return Qt(env);
}

// (spike-probe PATH) -> 中心の画素の色を文字列で返す（プロセス内で合成結果を読む）
static emacs_value Fprobe(emacs_env *env, ptrdiff_t n, emacs_value *a, void *d) {
  if (!g_child) return Qnil(env);
  NSView *content = [[g_child window] contentView];
  NSWindow *win = [content window];
  LOG(@"probe: child still in hierarchy = %d, subviews = %lu, hidden = %d",
      (int)([g_child superview] != nil), (unsigned long)[[content subviews] count],
      (int)[g_child isHiddenOrHasHiddenAncestor]);
  LOG(@"probe: child frame=%@ visibleRect=%@", NSStringFromRect([g_child frame]), NSStringFromRect([g_child visibleRect]));

  // ウィンドウ全体をプロセス内でビットマップに写す（画面収録の許可は要らない）
  NSRect cr = [content bounds];
  NSBitmapImageRep *rep = [content bitmapImageRepForCachingDisplayInRect:cr];
  [content cacheDisplayInRect:cr toBitmapImageRep:rep];
  NSPoint c = NSMakePoint(NSMidX([g_child frame]), NSMidY([g_child frame]));
  NSPoint cc = [content isFlipped] ? c : NSMakePoint(c.x, NSHeight(cr) - c.y);
  NSColor *col = [rep colorAtX:(NSInteger)(cc.x * [rep pixelsWide] / NSWidth(cr))
                             y:(NSInteger)(cc.y * [rep pixelsHigh] / NSHeight(cr))];
  NSString *s = [NSString stringWithFormat:@"rgb(%.2f,%.2f,%.2f)",
                 [col redComponent], [col greenComponent], [col blueComponent]];
  LOG(@"probe: pixel at center of child = %@ (rep %ldx%ld)", s, (long)[rep pixelsWide], (long)[rep pixelsHigh]);
  // PNG に保存
  NSData *png = [rep representationUsingType:NSBitmapImageFileTypePNG properties:@{}];
  ptrdiff_t len = 0; env->copy_string_contents(env, a[0], NULL, &len);
  char *path = malloc(len); env->copy_string_contents(env, a[0], path, &len);
  [png writeToFile:[NSString stringWithUTF8String:path] atomically:YES];
  free(path);
  return env->make_string(env, [s UTF8String], strlen([s UTF8String]));
}

// (spike-bg-update) -> 別スレッドから CALayer を更新できるか（Emacs の main loop に依存せず）
static emacs_value Fbg(emacs_env *env, ptrdiff_t n, emacs_value *a, void *d) {
  NSView *v = g_child;
  if (!v) return Qnil(env);
  dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INTERACTIVE, 0), ^{
    for (int i = 0; i < 20; i++) {
      [CATransaction begin];
      [CATransaction setDisableActions:YES];
      [[v layer] setBackgroundColor:CGColorCreateGenericRGB(0.1 + i * 0.04, 0.6, 0.2, 1.0)];
      [CATransaction commit];
      usleep(50 * 1000);
    }
    fprintf(stderr, "SPIKE: background layer updates done (main thread = %d)\n", (int)[NSThread isMainThread]);
  });
  return Qt(env);
}

// (spike-commits) 別スレッドで 1.2 秒間 CATransaction を commit し続け、時刻を記録する
static NSMutableString *g_commits;
static emacs_value Fcommits(emacs_env *env, ptrdiff_t n, emacs_value *a, void *d) {
  NSView *v = g_child;
  if (!v) return Qnil(env);
  g_commits = [NSMutableString string];
  dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INTERACTIVE, 0), ^{
    NSTimeInterval t0 = [NSDate timeIntervalSinceReferenceDate];
    int i = 0;
    while ([NSDate timeIntervalSinceReferenceDate] - t0 < 1.2) {
      NSTimeInterval a0 = [NSDate timeIntervalSinceReferenceDate];
      [CATransaction begin];
      [CATransaction setDisableActions:YES];
      [[v layer] setBackgroundColor:CGColorCreateGenericRGB((i % 20) * 0.05, 0.3, 0.7, 1.0)];
      [CATransaction commit];
      NSTimeInterval a1 = [NSDate timeIntervalSinceReferenceDate];
      if (i % 20 == 0)
        [g_commits appendFormat:@"  commit #%d at %.3f s, took %.2f ms\n", i, a1 - t0, (a1 - a0) * 1000];
      i++;
      usleep(5000);
    }
    [g_commits appendFormat:@"  commits total = %d in 1.2 s (main thread = %d)\n", i, (int)[NSThread isMainThread]];
  });
  return Qt(env);
}

// (spike-commit-log)
static emacs_value Fcommitlog(emacs_env *env, ptrdiff_t n, emacs_value *a, void *d) {
  const char *s = g_commits ? [g_commits UTF8String] : "";
  return env->make_string(env, s, strlen(s));
}

// (spike-hittest) 子ビューがマウスを横取りするか / NSApp のウィンドウ一覧
static emacs_value Fhit(emacs_env *env, ptrdiff_t n, emacs_value *a, void *d) {
  NSView *content = [g_child superview];
  NSPoint p = NSMakePoint(NSMidX([g_child frame]), NSMidY([g_child frame]));
  NSView *hit = [content hitTest:p];
  NSMutableString *s = [NSMutableString stringWithFormat:@"hitTest at center -> %@", [hit class]];
  [s appendFormat:@"\nNSApp.windows = %lu:", (unsigned long)[[NSApp windows] count]];
  for (NSWindow *x in [NSApp windows])
    [s appendFormat:@"\n  #%ld %@ title=%@ visible=%d level=%ld parent=%@",
     (long)[x windowNumber], [x class], [x title], (int)[x isVisible], (long)[x level], [[x parentWindow] title]];
  const char *c = [s UTF8String];
  return env->make_string(env, c, strlen(c));
}

static emacs_value Flog(emacs_env *env, ptrdiff_t n, emacs_value *a, void *d) {
  const char *s = [g_log UTF8String];
  return env->make_string(env, s, strlen(s));
}

static void def(emacs_env *env, const char *name, emacs_value (*f)(emacs_env *, ptrdiff_t, emacs_value *, void *), int mn, int mx) {
  emacs_value args[] = { env->intern(env, name), env->make_function(env, mn, mx, f, NULL, NULL) };
  env->funcall(env, env->intern(env, "defalias"), 2, args);
}

int emacs_module_init(struct emacs_runtime *ert) {
  emacs_env *env = ert->get_environment(ert);
  g_log = [NSMutableString string];
  def(env, "spike-attach", Fattach, 5, 5);
  def(env, "spike-probe", Fprobe, 1, 1);
  def(env, "spike-bg-update", Fbg, 0, 0);
  def(env, "spike-log", Flog, 0, 0);
  def(env, "spike-commits", Fcommits, 0, 0);
  def(env, "spike-commit-log", Fcommitlog, 0, 0);
  def(env, "spike-hittest", Fhit, 0, 0);
  emacs_value feat[] = { env->intern(env, "spike") };
  env->funcall(env, env->intern(env, "provide"), 1, feat);
  return 0;
}
