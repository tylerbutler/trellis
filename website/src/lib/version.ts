// The site derives the release version the same way trellis derives its
// workspace model: from the manifest that already exists. Cargo.toml is the
// single source of truth; no page carries a hand-maintained version string.
// The ?raw import inlines the manifest at build time, so the built site has
// no filesystem dependency and dev rebuilds track edits to Cargo.toml.
import cargoToml from "../../../Cargo.toml?raw";

const match = cargoToml.match(/^version\s*=\s*"([^"]+)"/m);

if (!match?.[1]) {
  throw new Error("Could not derive the trellis version from Cargo.toml");
}

export const TRELLIS_VERSION: string = match[1];
