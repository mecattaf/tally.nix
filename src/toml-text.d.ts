// Ambient module declaration for TOML files imported as TEXT (`import x from "./f.toml" with
// { type: "text" }`). Bun embeds the file's UTF-8 contents into the compiled binary and returns it as a
// string; TypeScript needs this declaration to type the import. The composition root
// (`src/compose.ts`) uses it to bundle the detector's `manifests/*.toml` into the single-file binary so
// `tally daemon run` classifies agent panes with no external manifest dependency.
declare module "*.toml" {
  const contents: string;
  export default contents;
}
