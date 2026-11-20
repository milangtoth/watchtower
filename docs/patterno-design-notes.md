# Patterno web design notes

Source: https://www.patterno.de/en, reviewed 2026-11-20. Values below are the site's own computed styles and CSS variables, read in the browser. Goal: make the Watchtower dashboard (`src/dashboard.html`) feel like it belongs to Patterno.

## Overall feel

- **Dark-first.** The site loads in dark mode (`<html class="dark">`) and has a sun/moon toggle in the nav. Light mode exists and is fully styled.
- **Deep navy-indigo, not black.** The page background is `#0a0a1a`, with soft indigo glows (radial gradients) behind the hero.
- **Very calm.** Big, thin headlines, lots of empty space, almost no hard lines. Borders are white at 5–8% opacity.
- **Product UI as illustration.** Instead of icons or stock graphics, each section shows a small fake product screen: a tender list, a document checklist, a timeline, an org chart. These are the closest thing to what our dashboard should look like.

## Colour tokens (their CSS variables)

| Token | Light | Dark |
|---|---|---|
| `--color-surface` | `#ffffff` (body is actually `#fafafa`) | `#0a0a1a` |
| `--color-surface-elevated` | `#f5f5f7` | `#0f0f2d` |
| `--color-text-primary` | `#1d1d1f` | `#ffffff` |
| `--color-text-secondary` | `#86868b` | `#ffffff99` (60% white) |
| `--color-border` | `#0000000a` (4% black) | `#ffffff14` (8% white) |
| `--color-accent` | `#6366f1` | `#6366f1` |

Other colours used:
- **Accent family (Tailwind indigo/violet):** `#6366f1` (accent), `#818cf8` (eyebrow labels, links), `#a5b4fc`, and muted lavender-greys `#6b6b9a`, `#7473b0` and `#62628e` for tertiary text.
- **Headline gradient:** `linear-gradient(90deg, #c4b5fd, #a78bfa, #818cf8)` on the last words of the H1 ("tender again.").
- **Dark text steps:** white at 100%, 90%, 70%, 65%, 60%, 50% and 40%. Hierarchy comes from opacity, not from different hues.
- **Status colours in the mock UIs:**
  - Emerald dot `#34d399` for live/good.
  - Amber for warnings ("Expires before deadline", "2 of 3 met").
  - Grey for pending ("Open").
- Named brand palette in their build: `patterno-navy-700/800/900` (`#0f0f2d`, `#0a0a1a`) and `patterno-purple-400/500`.

## Typography

- **Inter** (variable, 100–900) for everything in the UI.
- **EB Garamond** (serif) only for the "Patterno" wordmark, one feature headline ("A search that understands your business.") and the huge footer wordmark. The challenge brief PDF uses the same serif for its headings, so it's a brand signature.
- **Scale:**

| Role | Size / line height | Weight | Letter spacing |
|---|---|---|---|
| H1 | 68 / 81.6 | **300 (light)** | −1.36px (−0.02em) |
| H2 | 52 / 57.2 | 300 | −1.04px (−0.02em) |
| Garamond feature heading | 56 | 300 | |
| H3 / card title | 16 / 20 | 500 | |
| Lead paragraph | 18 / 30.6 | 400, at 60% white | |
| Body | 16 / 24 | 400 | |
| Eyebrow label | 12 / 18 | 500, uppercase, `#818cf8` | +2.16px (0.18em) |
| Buttons | 14 / 20 | 500 | |

- Light-weight big headlines, sometimes in two tones: the second line is dimmed, as in "One system / *for every subsidiary.*".
- `font-variant-numeric: tabular-nums` is used on numbers in the mock UIs (euro amounts, counts, days).

## Shape, depth, surfaces

- **Radius scale:** pill `9999px` (by far the most common: buttons, nav, tags, dots), 4, 6, 8, 10, 12, 14, 16 and 20px for cards. Large showcase panels use 32px.
- **Cards (dark):** transparent or very slightly lighter background, a `1px solid rgba(255,255,255,0.06)` border, 20px radius, no drop shadow.
- **Cards (light):** white, 14px radius, 16px padding, a soft shadow and a `0 1px 0 #fff inset` top highlight.
- **Floating nav:**
  - Fixed position, pill-shaped, 49px high.
  - Background `rgba(0,0,0,0.3)` with `backdrop-filter: blur(24px)` and a 5% white border.
  - Holds the logo, menus with chevrons, theme toggle, language switch, Login (ghost pill) and Book demo (white pill).
- **Glass chips** overlap the hero photo, for example "98% Match · IT services · €2.4M" and "€30M Contract volume this week". These are small dark translucent cards with a status dot.
- **Easing:** `--ease-spring: cubic-bezier(.25,1,.5,1)`.

## Buttons and small parts

- **Primary:** white pill, dark text (`#111827`), padding 8×20, weight 500. Often has a trailing `→`.
- **Secondary / ghost:** `rgba(255,255,255,0.1)` background, `1px rgba(255,255,255,0.2)` border, white text, pill.
- **Text link:** plain text with a `→`.
- **Tags:** small rounded labels such as `TED`, `DTVP`, `V24`, `BID` and `HIT` in uppercase on a slightly lighter chip. They're used for the portal/source name. **We can use this directly for portal software names.**
- **Status dot + label pill:** for example "● Bid Room" or "● 98% Match".
- **Checklist rows:** checkbox, then title over a grey subtitle, then a right-aligned status in a colour ("Pre-filled", "3 matching", amber "Expires before deadline", grey "Open"), with a "Progress 2 of 4 done" footer.

## Mock UI patterns worth copying for the dashboard

1. **Tender list card (HIT section):** rows with a source tag (`TED`), title, then a muted amount underneath. There's a floating counter chip ("4,500+ Portals"). **This works as our portal list:** software tag, portal name, tender count, state.
2. **Document analysis card (BID section):** file chip (PDF icon, name, "127 pages"), then key/value rows with checkmarks and right-aligned values, then a summary line ("Briefing ✓ Ready in 30 sec."). **This works as our portal detail page:** last read, tenders, documents, state reason.
3. **Timeline (Council resolutions):**
   - A month axis across the top.
   - One row per item, with bars, diamond markers and labels under them ("City council decision → Publication → Deadline").
   - A dashed bracket shows the span ("4 months head start").
   - **This works as our runs history:** reads and repairs as segments per portal, with failures marked.
4. **Org chart (subsidiaries):**
   - A parent card at the top with two big numbers and labels ("12 active bids · 3 subsidiaries").
   - Connector lines down to child cards.
   - Each child card has a name, a one-line description, avatars, then two big numbers ("5 active bids · in 6 days next deadline").
   - **This works as our summary strip:** serving / healing / failed counts.
5. **Stat tiles:** a big light number ("1 Day", "847", "€2.4M") with a small caption above and below.

## Layout

- Main container is about **1152px** wide (`max-w-6xl`), with some 1200px sections. Text blocks are capped at 512–672px.
- Section pattern: eyebrow label, then a big light H2, then a short 60%-opacity lead, then the visual. The H2 is often left-aligned with the lead text to its right on desktop.
- **Generous vertical spacing:** the homepage is about 13,500px tall for 11 sections.
- **Mobile (375px):**
  - The nav becomes logo + "Book demo" pill + hamburger.
  - Card grids drop to 2 columns, then 1.
  - The hero relies on entrance animations. On first paint on mobile the hero was **empty** until the animation ran. Avoid that: our content should be visible without JS animation.

## What to avoid / not copy

- Marketing extras: the sticky "Want to see Patterno in action?" bar at the bottom, a lead-capture modal that opens on scroll, the cookie dialog, logo walls and stock photography.
- Scroll-jacked / pinned sections. The site hijacks scrolling, and `scrollIntoView` didn't land where expected. An operator dashboard should scroll normally.
- Real Patterno logo files or the "Patterno" wordmark as our own branding. We're a challenge entry, so take the *style*, not the brand assets.

## Suggested direction for the Watchtower dashboard

- **Base:** dark-first `#0a0a1a` with `#0f0f2d` cards, 8% white borders, 20px card radius, Inter, plus a light theme from the same tokens. Copy the six `--color-*` variables above as our tokens.
- **Headings:** light weight (300) with tight negative tracking. A Garamond "Watchtower" wordmark would echo their brand without copying it.
- **State colours, all as dot + label pills:**
  - `serving`: emerald `#34d399`
  - `healing`: indigo `#818cf8`, pulsing
  - `integrating`: grey or 60% white
  - `failed`: a red we need to add (they show none; Tailwind `#f87171` fits the palette)
  - Use amber for warnings such as "documents missing".
- Portal software / source as small uppercase tags (like their `TED` / `DTVP` chips).
- Numbers in `tabular-nums`. Big light stat tiles for the summary counts.
- **Runs history** as their timeline pattern; **portal detail** as their checklist / key-value card.
