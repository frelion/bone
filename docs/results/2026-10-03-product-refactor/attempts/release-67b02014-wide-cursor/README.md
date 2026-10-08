# Superseded wide-caret observation

A final visual review of installed binary `67b02014` found a second reverse blank in the inactive parent draft behind API step 4/4. The intended parent `TextArea` buffer had no reverse caret, but the terminal retained a trailing cell from an earlier wide glyph.

A fresh short PTY reproduction passed through a restored selected Chinese reply, Ctrl+P, then “写新要求”. The previous normal wide glyph at 1-based row 22, column 54 was replaced by one reverse narrow blank. The emitted diff reset the style and continued at column 56, leaving column 55 reversed.

Independent Playwright-bundled xterm.js reproduced the same known ANSI behavior: overwriting a normal wide glyph with a reverse narrow space also clears its trailing cell with the current reverse style. The replay parser agrees; no parser correction was made. `observation.json` retains the original final form frame and minimal transition.

The production correction must explicitly refresh the actual following cell when drawing a narrow caret. The final product-flow adds an assertion that the empty inactive parent draft contains no reverse cell behind the API form.
