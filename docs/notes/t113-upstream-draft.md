# T-113 libghostty-vt の上流への報告の下書き（`CSI 0 S` / `CSI 0 T`）

- 日付: 2026-11-12
- **送っていない。** 送るかどうかと、送る先（GitHub の ghostty-org/ghostty の issue / discussion）は利用者が決める
- 根拠: docs/notes/t055-vttest.md §2.2（vttest 11.2 の SU / SD）

---

**Title:** SU / SD with an explicit parameter of 0 (`CSI 0 S` / `CSI 0 T`) does nothing instead of scrolling one line

**Body:**

libghostty-vt 1.0.0 (C ABI) treats `CSI 0 S` (SU, scroll up) and `CSI 0 T` (SD, scroll down) as a no-op.
xterm and ECMA-48 treat a parameter of 0 as the default (1), so these sequences scroll by one line there.
`CSI S` / `CSI 1 S` (and the `T` forms) work as expected.

Steps to reproduce (vttest 2.7, menu 11 → 2 → 2 "Test Pan down (SD)" / "Test Pan up (SU)"), or directly:

```
printf '1\r\n2\r\n3\r\n\e[0S'   # expected: the screen scrolls up one line; actual: nothing happens
```

We found this while running vttest against an embedded terminal built on libghostty-vt
(the same result when feeding the bytes to `ghostty_terminal_vt_write` directly, without ConPTY).

Expected: `CSI 0 S` behaves like `CSI 1 S` (xterm's `CSI Ps S`: "Scroll up Ps lines (default = 1)", and ECMA-48 §8.3.147 / §8.3.113
where a parameter value of 0 means the default).
