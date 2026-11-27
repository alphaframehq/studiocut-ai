# GitHub Pages setup — StudioCut AI

This document explains how to publish the static site in `docs/` with
GitHub Pages. Enabling Pages is a repository-settings action performed in
the GitHub web UI; it is not done by a commit, and it is not done yet.

## What is published

- Source: the `docs/` folder on the `main` branch (`docs/index.html` and
  `docs/style.css`, plus this file and an empty `docs/.nojekyll` file that
  tells GitHub Pages to serve the files as-is, without Jekyll processing).
- The site is a plain static site: no build step, no dependencies, no
  external fonts or images.

## Steps to enable Pages

1. Merge the branch that adds `docs/` (`feature/github-pages-site`) into
   `main`. The site only goes live after this branch is merged and Pages
   is enabled — merging alone does not publish anything.
2. Open the repository on GitHub:
   <https://github.com/alphaframehq/studiocut-ai>
3. Go to **Settings** (the repository settings tab).
4. In the left sidebar, under **Code and automation**, click **Pages**.
5. Under **Build and deployment**, set **Source** to **Deploy from a branch**.
6. Set **Branch** to **main** and **Folder** to **/docs**.
7. Click **Save**.
8. Wait for the Pages deployment to finish (usually under a minute). The
   published URL is shown at the top of the Pages settings page, and is
   normally:
   <https://alphaframehq.github.io/studiocut-ai/>

## After enabling

- Every later push to `main` that changes files under `docs/` redeploys
  the site automatically.
- Verify the live site by opening the URL above and confirming the hero
  status text, the Features section, the Screenshots placeholder, the
  Download section and the Support link all render.
- Do not add installer file names or download buttons to the site until
  v0.1.0 installers are actually published on the Releases page
  (<https://github.com/alphaframehq/studiocut-ai/releases>).
