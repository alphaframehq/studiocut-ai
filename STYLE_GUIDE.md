# StudioCut AI — Style Guide & UI Kit (VIP Premium)

One dark studio theme. One token source. No new hex values in component CSS.

Source of truth: `src/design-tokens.css` — import it first in `src/main.tsx`,
before every other stylesheet. If a value is not a token, it does not belong
in the UI.

## Principles

1. **Dark studio, mint brand.** Background `#111516`, surfaces `#181d1e` /
   `#1d2324`, mint accent `#b8e4cd`. Gold `#e8c87a` is reserved for
   premium/AI highlights — use it sparingly, never as a second primary.
2. **One theme everywhere.** The Analysis panel used to be light lavender and
   the Finishing panel blue-grey while the rest of the app was dark green.
   That is fixed: all panels now use the same dark tokens. Section accents
   are allowed (violet for Analysis/captions), but never a second theme.
3. **Content keeps its colors.** Caption presets (Hormozi yellow, Neon glow,
   …), clip colors and LUT previews are content, not chrome — they keep
   their own colors.
4. **Readable by default.** Body copy is 12–13px. 9–11px is only for
   eyebrows, tags and micro-labels. Never write 8–10px body text in a new
   component (the old styles still have some — see "Still to migrate").
5. **Focus is always visible.** Every interactive element gets the mint
   `:focus-visible` ring (`--color-focus`, 2px + offset). No exceptions.

## Tokens (`src/design-tokens.css`)

| Group | Key tokens | Notes |
|---|---|---|
| Surfaces | `--color-bg`, `--color-bg-deep`, `--color-surface`, `--color-surface-elevated`, `--color-surface-sunken`, `--color-input`, `--color-input-strong` | Inputs are always `--color-input` (`#101514`); dialog inputs `--color-input-strong` |
| Borders | `--color-border`, `--color-border-strong`, `--color-border-accent` | Accent border is for badges/active tabs only |
| Text | `--color-text`, `--color-text-soft`, `--color-text-muted`, `--color-text-faint` | Muted `#9aa6a4` is the floor for secondary text |
| Accents | `--color-accent` (mint), `--color-accent-strong`, `--color-accent-ink`, `--color-accent-soft`, `--color-accent-dim`, `--color-gold`, `--color-violet` | Text on filled mint uses `--color-accent-ink` (`#1b3025`) |
| Feedback | `--color-focus`, `--color-error`, `--color-error-strong`, `--color-warning-bg/border/text` | Warnings are amber-on-dark, never yellow-on-white |
| Timeline | `--color-clip-video/border`, `--color-clip-audio/border`, `--color-clip-caption-bg/border`, `--color-playhead`, `--color-track` | |
| Type | `--text-eyebrow` (9) → `--text-hero` (25); `--font-sans` (Inter), `--font-mono` | |
| Space / radii / shadow / motion | `--space-1`…`--space-8`, `--radius-sm/md/lg/xl/dialog`, `--shadow-card/dialog/pop`, `--motion-fast/base` | |

Legacy aliases `--muted`, `--line`, `--mint` still exist for old rules; new
code uses the `--color-*` names.

## Components

### Buttons
- `.button.primary` — filled mint, `--color-accent-ink` text, hover
  `--color-accent-strong`. The one main action per view.
- `.button.secondary` — `--color-surface-elevated` bg, `--color-border-strong`
  border, `--color-text` text. Hover lightens the surface.
- `.toggle-button` / `.toggle-button.on` — panel toggles; `.on` is
  `--color-accent` text on a tinted surface with `--color-border-accent`.
- `.icon-button` — 32–34px square, transparent, hover surface. Always has an
  `aria-label`.
- Disabled: `opacity: .45`, `cursor: not-allowed`. Pressed buttons translate
  1px down (`:active`).

### Inputs, selects, textareas
- Background `--color-input`, border `--color-border-strong`, radius
  `--radius-md`, padding 9–11px, text `--color-text`, font inherits.
- Focus: 2px `--color-focus` outline, offset 2–3px.
- Labels are 11–12px, `--color-text-soft`, stacked above the field with a
  5–7px gap. Never put a bare input without a label.

### Dialogs (`<dialog>`)
- Background `--color-surface`, border `--color-border-strong`, radius
  `--radius-dialog` (14px), shadow `--shadow-dialog`.
- Backdrop: near-black with blur (`#050907bb` + `blur(4px)`).
- Header: eyebrow (`--text-eyebrow`, letterspaced, muted) + title
  (`--text-hero`, tight tracking). Footer notes are muted 11–12px.

### Tabs
- Row of text buttons, 11–12px, muted; selected tab is
  `--color-accent` text with a 2px accent underline. `role="tablist"` /
  `aria-selected` required.

### Cards & panels
- Panel: `--color-surface` bg, `--color-border` 1px, radius `--radius-xl`,
  shadow `--shadow-card` for the workspace card.
- Media card: `--color-surface-elevated`-ish `#202927` (still to migrate),
  radius `--radius-md`, thumbnail well `--color-surface-sunken`.
- Section labels (eyebrows): 8–9px, letterspaced, `--color-text-muted`.

### Timeline
- Track heads sticky left (150px), lanes `--color-track` tint, ruler 30px.
- Clips: video `--color-clip-video`, audio `--color-clip-audio`, captions
  `--color-clip-caption-bg`; selected clip gets a 2px `--color-playhead`
  outline. Playhead is a 1px `--color-playhead` line.
- Touch: clips/handles use `touch-action: none`; min tap target 42px on
  mobile (≤700px breakpoint).

### Status, errors, empty states
- Live messages: `role="status"` (mint-ish) / `role="alert"` for errors
  (`--color-error`).
- Empty states are centered, icon + heading + one line + one action. Never
  an empty grey box.

## Layout & breakpoints
- App shell: 72px header → 70px icon rail → centered workspace
  (`max-width: 1550px`).
- `≥1500px`: media column widens to 300px. `≤1000px`: rail/grid narrow.
  `≤700px`: rail hides, heading stacks, editor grid becomes
  `column-reverse` (preview above media bin), media list scrolls
  horizontally, tap targets grow to 42px+.
- Panels collapse: Analysis 3 → 2 → 1 column at 1050/600px; Director
  2 → 1 column at 760px; finish grid 3 → 1 at 600px.
- Motion: `prefers-reduced-motion: reduce` disables transitions/transforms.

## Do / Don't
- Do: use a token; reuse `.button`, `.tabs`, `.settings-dialog` patterns.
- Don't: invent a hex, a second accent, a light panel, or 8px body text.
- Don't: style a one-off button — extend the kit and note it here.
- Don't: put secrets, paths or raw errors in UI copy; errors are short,
  human sentences (`role="alert"`).

## Still to migrate (gradual refactor backlog)
- `src/styles.css` still has ~90 nuanced hard-coded shades (track greens,
  text variants like `#a5b2a9`, hover states). They are visually close to
  tokens; map them to a small set of step tokens (`--color-text-step-*`,
  `--color-surface-step-*`) in a follow-up pass — do not bulk-replace by eye.
- A few 8–10px body strings remain in the old styles; raise them to 11–12px
  when their component is next touched.
- Tailwind is installed but unused — either adopt its utilities for new
  components or remove the dependency; do not mix both systems silently.
- The footer step label is still hard-coded (`STEP 03 / TIMELINE & PREVIEW`).

## How to add a component
1. Add tokens first if a new value is needed (never a raw hex).
2. Build it from kit primitives (button/input/dialog/tab/card).
3. Add it to this guide with its class names and states.
4. Check it at 390px and 1280px, with keyboard only, and with
   `prefers-reduced-motion` on.
