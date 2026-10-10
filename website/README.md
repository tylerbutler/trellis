# Trellis website

The Astro and Starlight site builds to `dist/`. Cloudflare Workers serves these
static assets; no SSR adapter or Worker entry point is needed.

## Local development

Use Node.js 24 and pnpm 11.9.0. Run these commands from `website/`:

```sh
pnpm install --frozen-lockfile
pnpm dev
```

To preview the production build with Workers routing:

```sh
pnpm build
pnpm exec wrangler dev
```

`wrangler.jsonc` serves directory index pages with trailing slashes and uses the
generated `404.html` for missing pages.

## Automatic deployments

Use [Workers Builds](https://developers.cloudflare.com/workers/ci-cd/builds/)
to connect `tylerbutler/trellis` in the Cloudflare dashboard. Create a Worker
named `trellis-website`; its name must match `wrangler.jsonc`.

| Build setting | Value |
| --- | --- |
| Production branch | `main` |
| Root directory | `website` |
| Build command | `pnpm run build` |
| Deploy command | `pnpm exec wrangler deploy` |
| Preview command | `pnpm exec wrangler preview` |
| Build variable | `PNPM_VERSION=11.9.0` |

Workers Builds installs dependencies before running the build command.
`.node-version` selects Node.js 24. Set `PNPM_VERSION` to match the
`packageManager` version in `package.json`; the workspace uses pnpm's
`allowBuilds` settings for native dependencies.

Enable builds for non-production branches to replace Netlify deploy previews.
The preview command does not deploy to production. Cloudflare manages the build
API token, so this setup does not need GitHub Actions deployment secrets.

For a manual deployment from `website/`, run `pnpm exec wrangler login`, then
`pnpm deploy`. This builds the site before deploying it to production.

## Switching from Netlify

Keep Netlify active until the Worker is ready:

1. Deploy to the Worker's `workers.dev` URL. Check the home page, docs, search,
   redirects, and an unknown path that must return the custom 404 page.
2. Confirm that `tylerbutler.com` is an active zone in the same Cloudflare
   account. Record the existing Netlify DNS target for rollback.
3. Remove any existing CNAME for `trellis.tylerbutler.com`, then add that hostname
   under the Worker's **Settings > Domains & Routes > Add > Custom Domain**.
   Cloudflare creates the DNS record and certificate. An existing CNAME prevents
   [Custom Domain setup](https://developers.cloudflare.com/workers/configuration/routing/custom-domains/).
4. Check `https://trellis.tylerbutler.com` over HTTPS and repeat the site checks.
   The canonical URL in `astro.config.mjs` stays unchanged.
5. Disable Netlify builds after the domain works. Retain the Netlify site until
   the rollback window ends.

Custom Domain setup is separate from the initial deployment so a test deploy
does not switch production traffic. To roll back, remove the Worker's Custom
Domain and restore the recorded Netlify DNS target.
