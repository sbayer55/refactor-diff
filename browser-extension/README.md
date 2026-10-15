# Open in Refactor Diff (Chrome extension)

Adds an **Open in Refactor Diff** button to GitHub pull request pages
(`/owner/repo/pull/123`, including the Files and Commits tabs) and compare pages
(`/owner/repo/compare/base...head`). Clicking it opens a `refactor-diff://` link,
which the Refactor Diff macOS desktop app handles by opening that diff.

Plain JavaScript with no dependencies. The extension asks for no
permissions: it is a content script on `https://github.com/*` and nothing else.

Cross-fork compares (`main...someone:branch`) aren't supported yet; the button is
shown disabled there. Open the pull request instead.

## Install

1. Run `just extension-build`. It copies the files Chrome needs (no tests or README)
   into `target/browser-extension/`.
2. Open `chrome://extensions` and turn on **Developer mode** (top right).
3. Click **Load unpacked** and select `target/browser-extension/`.

The first time you click the button, Chrome asks **"Open Refactor Diff?"**. Tick
**Always allow** so it doesn't ask again. The desktop app must be installed for the
link to work.

After editing the files, run `just extension-build` again, click the reload icon on
the extension's card and refresh the GitHub tab.

## Link format

Values are percent-encoded with `URLSearchParams`:

```
refactor-diff://open?repo=OWNER/NAME&pr=123
refactor-diff://open?repo=OWNER/NAME&base=main&head=feature/x
refactor-diff://open?repo=OWNER/NAME&head=feature/x
```

Without `base`, the app picks its default base. For example, head `feature/x` is
sent as `head=feature%2Fx`.

## Files

- `manifest.json`: Manifest V3 definition.
- `github-url.js`: pure functions that parse a GitHub pathname and build the link
  (`parseGitHubLocation`, `buildOpenUrl`). Works as a content script and from Node.
- `content.js`: places the button in the PR header or next to the compare range
  editor, or as a floating button bottom-right when neither is found, and keeps it
  in sync as GitHub navigates without full page loads.
- `content.css`: button, disabled and floating styles.

## Tests

```
just extension-test
# or
node --test browser-extension/*.test.js
```

`just extension-zip` builds the extension and writes
`target/refactor-diff-extension.zip` for sharing.
