- Do NOT write ANY compatability shims, backwards compatability, migration code, or anything that could possibly 
be construed as anything similar to this in any way
- HARD CUTS ONLY
- No fallbacks. Fallbacks are a code smell. Either we coded things right and are adhering to the spec, or we didn't. 
- This is not the same thing as not handling edge cases, or having graceful failure conditions around external interfaces like discord etc. These are not fallbacks.
- We should not treat our own codebase as hostile. Checking an incoming parameter is valid or mutating it to fit a certain spec is stupid. We own the codebase, we own the contracts, we do not need to be defensive against ourselves.
- Do not frame the hierarchical parent/child job architecture as the root cause of latency. If a parent/child transition is slow, identify the specific inefficient operation behind it: lock contention, unnecessary blob fetch/decode, storage contention, sleeps/timers, API calls, or other concrete causes.
- All tests go in tests/, not inline with the source file
- When making a change, check if docs/ needs to be updated


## Documentation writing rules

- Treat the Rust code as the final authority. Documentation must describe what the code does now.
- Write in present tense. Avoid development-history narration, previous-plan framing, and references to conversations the reader was not part of.
- Write authoritatively and directly. Prefer concrete technical statements over rhetorical framing.
- Make documentation read as connected narrative where appropriate: use paragraphs, concepts, flows, and diagrams. Use bullets, tables, and code references only when they clarify the material.
- Keep code references light. Point to primary modules or surfaces when useful, but do not turn docs into a pile of file citations.
- Avoid negative concept framing such as "the runtime is not..." or "the system should not..." in user-facing docs. State the active model directly.
- Avoid "not X, but Y" constructions and similar contrast filler. Name the thing and describe it.
- Avoid filler statements that add no technical value, such as claims that the runtime "stays honest" or that a section is "a story rather than notes."
- Style edits must preserve technical truth. Do not replace precise architecture language with a simpler label unless the code supports that label.
- When documenting latency, describe the concrete operation involved: locks, blob fetch/decode, storage contention, sleeps/timers, API calls, provider calls, scheduler ordering, or another measurable cause.
