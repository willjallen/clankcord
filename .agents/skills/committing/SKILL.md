---
name: committing
description: Use this skill when preparing commit messages or splitting changes into commits for this repository. Do not use this skill for build, performance benchmarking, or screenshot-capture workflows.
---

# Committing

All commits must use the schema:

[category/<kind>] brief summary

- Point 1
- Point 2 (optional)
- Point 3 (optional)

OR

[category/<kind>] brief summary

Long summary, in paragraph form.

----

First line of the commit message is always in lower case.

Categories: Here are some non-exhaustive examples. Try to first reuse an existing category in the commit history unless it doesn't make sense
- ai
- git
- ux
- agent
- runtime
- core
- stt
- lib
- css
- etc.
- Invent new ones as appropriate

Note that the category may be ambiguous because the change covers multiple files.
Generally speaking, pick the category that the code change was originally in service of.
If this is still too ambiguous, break the commit into two or more but YOU MUST ENSURE
- Each commit compiles and runs independently
- Each commit can stand on its own as a logical change.

Kinds: Not strictly required, but should be present 95% of the time
- feat:     A new feature.
- fix:      Fixing something to work as intended.
- docs:     Documentation changes.
- style:    Code style changes (formatting, missing semicolons, etc.).
- refactor: Code refactoring (neither fixes a bug nor adds a feature).
- nit:      Code refactoring that is pedantic or minor. Not worth of "refactor" label
- test:     Adding or updating tests.
- chore:    Routine tasks like updating dependencies or build tools.
- build:    Changes affecting the build system or external dependencies.
- ci:       Changes to CI configuration files or scripts.
- perf:     Performance improvements.
- revert:   Reverting a previous commit.

## Commit message safety checks

- Always include a blank line between the first line (subject) and any following bullet points:
  - `[x/y] summary`
  - `<blank line>`
  - `- a`
  - `- b`
- Never put literal `\n` sequences inside a `-m "..."` string expecting Git to turn them into new lines. Git stores them literally.
- If you want multiple bullet lines, pass multiple `-m` flags (one per paragraph/line block), or use `-F` with a prepared message file.

## Practical commit command flow (recommended)

1. Verify and group changes.
   - `git status --short`
   - `git diff -- <path>`
2. Stage only the files for one logical commit.
   - `git add <file1> <file2> ...`
3. Sanity-check staged content.
   - `git diff --cached`
4. Commit with explicit paragraphs (safe multiline form).
   - `git commit -m "[ui/fix] brief summary" -m "- point 1" -m "- point 2"`
5. Validate final message formatting.
   - `git log -n 1 --format=%B`

For longer messages, prefer:

```bash
cat > /tmp/commit-msg.txt <<'MSG'
[ui/fix] brief summary
- Point 1
- Point 2
MSG
git commit -F /tmp/commit-msg.txt
```
