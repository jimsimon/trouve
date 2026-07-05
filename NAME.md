# Why "trouve"?

**trouve** is pronounced **"troov"** — rhymes with *groove*, French /tʁuv/.
It is the imperative of the French verb *trouver*: **"find."**

## A nod to semble

trouve began as a Rust port of [MinishLab/semble](https://github.com/MinishLab/semble),
whose name comes from French *sembler* — "to seem." A semantic search engine
returns what *seems* relevant; ours returns what it *finds*. Keeping the name
French, single-word, and verb-shaped honors the upstream project while marking
the port as its own thing: semble suggests, trouve retrieves.

## Significance for the search tool

For a code search tool the fit is literal. Everything the tool does is a form
of finding:

- `search` finds the chunks of a codebase relevant to a query.
- `find-related` finds code connected to a given file and line.
- The content-addressed store *finds* previously computed chunks, embeddings,
  and BM25 rows by hash instead of recomputing them — the incremental design
  is itself "find, don't rebuild."

The command reads as an instruction: *trouve* — "find (it)."

## Significance as an AI umbrella

The `@trouve-ai` npm scope is an umbrella under which the search tool
(`@trouve-ai/search-core`, `@trouve-ai/search-plugin`) is the first product.
Two properties make the name stretch beyond search:

**Finding is the central metaphor of practical AI.** Retrieval, discovery,
pattern recognition, question answering, recommendation, anomaly detection —
most of what AI systems do for people is surface the thing that matters from
a space too large to scan by hand. "Eureka" — the archetypal word for insight
— literally means *I have found it*. A name that means "find" covers most of
the useful AI product space without naming any single technique.

**The word's own history spans retrieval *and* creation.** The leading
etymology traces *trouver* to Vulgar Latin *tropāre*, "to compose, to
invent" — the same root as *troubadour* and *trouvère*, the medieval poets
who were literally "finders" (inventors, composers) of verse. In its history
the word means both *to find* and *to create*. Modern AI lives on exactly
that line: retrieval on one side, generation on the other. Few words carry
both senses natively; *trouver* does.

So the umbrella stays abstract and evocative — *find/create* — while each
product name carries the specificity (`search-core`, `search-plugin`, and
whatever comes next).

## Naming conventions

- **Brand / umbrella:** trouve (lowercase in prose, like the CLI).
- **npm scope:** `@trouve-ai`.
- **Crate and binary:** `trouve-search` — the product, namespaced under the
  brand.
- **Product packages:** `@trouve-ai/<product>-…` (e.g. `search-core`,
  `search-plugin`, per-platform binary packages).
- **Environment variables:** `TROUVE_*`.
- **Ignore file:** `.trouveignore`.
