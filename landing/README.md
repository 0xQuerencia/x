# X landing (React + TypeScript)

This replaces the standalone JavaScript UI with typed React components while keeping the existing page copy, original CSS, images, image geometry, typography, screenshots, navigation, and dark/light theme key (`x-theme`) unchanged. The separate Hex website at `site/` is unrelated.

## Develop

Requires Node 22+.

```sh
cd landing
npm install
npm run dev
npm run check      # strict TypeScript check + Vite production build
npm run preview
```

## Deploy to x.querencia.art

Vite outputs static files to `landing/dist/`. In your hosting provider's build configuration, use root directory `landing`, build command `npm install && npm run build`, and output directory `dist` (Node 22+). Retain the existing domain and DNS configuration. The `public/img/` tree mirrors the original image assets so existing relative `img/` paths still work.

**Important:** If your host currently publishes `landing/` directly without a build step, update its build settings *before* deploying this branch. Browsers cannot run TSX from `index.html` unbuilt. This repository has no workflow that deploys X's landing automatically; the workflows in `site/` publish the unrelated Hex site.

Check on mobile and desktop before rollout: both themes, hero screenshot tabs and morph animation, install tabs and copy button, carousel, FAQs, navigation, animations and reduced-motion setting. The visual stylesheet was not touched.
