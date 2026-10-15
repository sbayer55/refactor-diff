# File history mockups

Five interactive prototypes for an animated file-history view in refactor-diff. Each one
covers the same three jobs, in a different presentation:

- step backward and forward through a file's whole history
- check out the commit of the version in view (whole commit, or just this file)
- compare any two versions, not only neighbours

All five share `shared/history.js` (a sample history of `server/users.py` across nine
commits, a line diff, per-line blame, a tiny Python highlighter, and the line-morph
animation) and `shared/theme.css` (the app's own tokens, dark first with a light variant).

| File | Where it lives | Animation | Compare any two |
| --- | --- | --- | --- |
| `1-scrub.html` | inside the file viewer, as a History mode | drag a playhead; lines collapse and expand in place | mark A, move B; range shaded on the track |
| `2-replay.html` | full screen | play history like video; recently written lines glow and cool | in/out points, like a video editor |
| `3-deck.html` | full screen | versions as cards in a 3D deck with a thumbnail rail | pin a card as A; the front card shows the diff |
| `4-strata.html` | inside the file viewer, a rail beside the code | hover ghosts the diff; click wipes the version in from the top | shift-click a layer to set A |
| `5-dual-cursor.html` | full screen | two cursors on one timeline; ribbons connect differing hunks | both cursors are free; lock the gap to step them together |

Every design also colours each line by age (blame) and lets you jump to the commit that
wrote a line from the gutter.

To view locally, serve this directory (the pages load the shared files relatively):

```bash
python3 -m http.server 8765 -d docs/mockups/file-history
```

## Round two

After review, five more designs share one core: a vertical commit list (compact or tucked away while
comparing), scroll that follows the line you are on, folds for both unchanged context and individual
changes (nothing folded by default), and one screen where pressing `p` pins A and the view splits with
the older version always on the left. They share `shared/engine2.js` and `shared/engine2.css`.

| File | Compare presentation | What is different |
| --- | --- | --- |
| `6-spine.html` | aligned side by side | hover ghosts the diff in place; the list shrinks to a spine when split |
| `7-hybrid.html` | inline split | unchanged code stays full width, only changed blocks open into two columns; hover previews exactly that |
| `8-ribbons.html` | unaligned side by side | each side keeps true line numbers; ribbons tie differing blocks; sync scroll follows the ribbons; minimaps per pane. **Chosen direction**, refined: dismissible tips, sticky ghost (space), blame tips in the gutter, folds in single and split modes, resizable and hideable commit list, ribbon toggle for small screens, horizontal scroll with sync, soft wrap; toggles persist in settings |
| `9-ladder.html` | aligned side by side | history starts from the code: every function or class carries a chip that unfolds its earlier versions in place |
| `10-curtain.html` | curtain | the older version sits behind a draggable curtain over the newer one, rows aligned |
