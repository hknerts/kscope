# Packaging

Templates for the package managers kscope ships through, and the script that
fills them in. Each release runs [`render.sh`](render.sh) (job `manifests` in
`.github/workflows/release.yml`) and attaches the results to the release page:

| Release asset | Template | Goes to |
| --- | --- | --- |
| `kscope.rb` | [`homebrew/kscope.rb.in`](homebrew/kscope.rb.in) | `Formula/kscope.rb` in the tap repo `hknerts/homebrew-tap` |
| `kscope.yaml` | [`krew/kscope.yaml.in`](krew/kscope.yaml.in) | `plugins/kscope.yaml` in [krew-index](https://github.com/kubernetes-sigs/krew-index) |
| `kscope.json` | [`scoop/kscope.json.in`](scoop/kscope.json.in) | `bucket/kscope.json` in a Scoop bucket, e.g. `hknerts/scoop-bucket` |

Nix needs no rendering: [`flake.nix`](../flake.nix) at the repository root
builds from source.

Render any published release locally (it fetches the `.sha256` assets):

```sh
packaging/render.sh 1.7.0 dist/
```

## One-time setup

1. **Homebrew:** create the public repository `hknerts/homebrew-tap` and
   commit the rendered `kscope.rb` as `Formula/kscope.rb`. Then
   `brew install hknerts/tap/kscope` works. On each release, replace the file
   with the new `kscope.rb` asset.
2. **krew:** the archives must contain `LICENSE`, which they do from the first
   release after 1.7.0. Test with
   `kubectl krew install --manifest=dist/kscope.yaml`, then open a pull request
   adding it to krew-index as `plugins/kscope.yaml`. Later versions can be
   bumped automatically by
   [krew-release-bot](https://github.com/rajatjindal/krew-release-bot).
3. **Scoop:** create `hknerts/scoop-bucket` with the rendered `kscope.json` in
   `bucket/`. Then `scoop bucket add hknerts https://github.com/hknerts/scoop-bucket`
   and `scoop install kscope`. The manifest has `checkver` and `autoupdate`,
   so Scoop's own update tooling can bump it.
4. **Nix:** nothing to publish. Run `nix flake lock` once and commit
   `flake.lock`, so `nix run github:hknerts/kscope` is reproducible.
   Getting into nixpkgs is a separate pull request to NixOS/nixpkgs.
