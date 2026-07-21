# Compaction and Branch Summaries

Bone compacts older conversation history when the context approaches the active model's limit. Use `/compact` to request compaction manually. Bone keeps recent messages and replaces older history with a summary so the conversation can continue within the model context window.

When navigating with `/tree`, Bone can summarize the branch being left and add that context to the selected branch. Session files retain compaction and branch-summary entries for later inspection.

Compaction settings such as `compaction.enabled`, `reserveTokens`, and `keepRecentTokens` can be configured in Bone settings. Bone does not expose third-party hooks for custom compaction behavior.
