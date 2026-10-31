// Berth's version, as package.json says (bumped with the app's; the bundle
// inlines it): what Claude Code and the berth MCP server are told.
import pkg from "../package.json" with { type: "json" };

export const VERSION: string = pkg.version;
