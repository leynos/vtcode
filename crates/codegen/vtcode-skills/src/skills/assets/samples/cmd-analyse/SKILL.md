---
name: cmd-analyse
description: "Perform comprehensive codebase analysis and generate reports (usage: /analyse [full|security|performance])"
disable-model-invocation: true
metadata:
  slash_alias: "/analyse"
  usage: "/analyse [full|security|performance]"
  category: "tools"
  backend: "traditional_skill"
---

# Analyse Workspace

Interpret the user input as the raw argument string that follows `/analyse`.

- If the input is empty, perform a full workspace analysis.
- If the input is `full`, `security`, or `performance`, focus on that scope.
- Base the analysis on the current workspace contents, not generic advice.
- Call out concrete findings, risks, and prioritized next actions.
- Keep the response concise and actionable.
