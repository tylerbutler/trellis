// Dismissal state for the announcement bar. The stored value is the version
// that was dismissed, not a boolean: silencing v0.6.0 says nothing about
// v0.7.0, so the next release surfaces the bar again for everyone.
export const ANNOUNCE_STORAGE_KEY = "trellis:announce-dismissed";
