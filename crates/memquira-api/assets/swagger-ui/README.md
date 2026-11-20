# Vendored Swagger UI 5.32.14

`GET /docs` serves these files (`src/routes/swagger_ui.rs`). They replace `utoipa-swagger-ui` 9, whose build script
depended on the vulnerable `zip` 3 crate. The build needs no download and no unpacking.

## Provenance

- Source: the npm package `swagger-ui-dist@5.32.14` from the official registry
  (`https://registry.npmjs.org/swagger-ui-dist/-/swagger-ui-dist-5.32.14.tgz`).
  - Registry integrity `sha512-nOA2pSQhcmODMUQZpJHYKNuwniDUqcOWGNaSCOoZv12FdOSJ9JxV95HtyRGNMqEBj6h6lCNTy20TgZDYTSuUIg==`.
  - Tarball sha256 `609702d791d8d3cdcbc3a52632f6be2f9b743eadf6ba49ca9737dac2a6e0b2a3`; published by `swagger-api`;
    `gitHead` `6e8ce248db64190e4113676aba996943c56f2491`.
  - Swagger UI 5.32.14 is the first release that publishes the bundles' extracted licence files (upstream issue
    #8317). 5.17.14, used until then, did not ship them.
- These package members are copied byte for byte:
  - the 18 static files: the 5.17.14 set, plus `oauth2-redirect.js`, which `oauth2-redirect.html` now loads;
  - `LICENSE` and `NOTICE`;
  - the four `*.js.LICENSE.txt` files, one for each bundle that names it in its first-line banner.
- Not copied: the npm module glue (`index.js`, `absolute-path.js`, `package.json`, `README.md`) and the build logs
  (`log.*.txt`).
- `swagger-initializer.js` is the file `utoipa-swagger-ui` 9.0.2 served:
  1. its build script removed `layout: "StandaloneLayout"` from the upstream initializer;
  2. it replaced the first `url: … deepLinking: true,` block with a `{{config}}` placeholder;
  3. at request time it substituted the configuration `{"dom_id": "#swagger-ui", "url": "/openapi.json",
     "deepLinking": true, "layout": "StandaloneLayout"}`.

  The upstream initializer of 5.32.14 is byte-identical to that of 5.17.14 (sha256 `a895034f…`), so the file is
  unchanged.
- `SHA256SUMS` lists every file. A unit test checks the embedded bytes against it. `.gitattributes` disables line-ending
  conversion for this directory.
- To re-create: unpack the `swagger-ui-dist` npm tarball of the pinned version, copy the files listed in `SHA256SUMS`
  and regenerate the checksums.

## Licence

Swagger UI is licensed under the Apache License 2.0 (`LICENSE`), copyright SmartBear Software Inc. (`NOTICE`).

Its minified bundles include third-party JavaScript. The licence notices of those libraries, as published by Swagger
UI, are in the `*.js.LICENSE.txt` files.

Distributions of memcore that include these files must keep `LICENSE`, `NOTICE` and the `*.js.LICENSE.txt` files with
them. Binary distributions must also reproduce the `NOTICE` attribution, because the files are compiled into
`memcore-api`. The image and the release archives ship all six files.
