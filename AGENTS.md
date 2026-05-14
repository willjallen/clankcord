- Do NOT write ANY compatability shims, backwards compatability, migration code, or anything that could possibly 
be construed as anything similar to this in any way
- HARD CUTS ONLY
- No fallbacks. Fallbacks are a code smell. Either we coded things right and are adhering to the spec, or we didn't. 
- This is not the same thing as not handling edge cases, or having graceful failure conditions around external interfaces like discord etc. These are not fallbacks.
- We should not treat our own codebase as hostile. Checking an incoming parameter is valid or mutating it to fit a certain spec is stupid. We own the codebase, we own the contracts, we do not need to be defensive against ourselves.