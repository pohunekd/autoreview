---
kind: fixed
---
`publish.ai_signed_pattern` decides which discussion comments count as machine-written and what `publish.signature` must start with, leading symbols such as an emoji ignored; the fixed text `Written by AI:` no longer applies to other patterns. A pattern with no text after its leading symbols, such as `🤖`, is refused as `configuration_invalid` naming `publish.ai_signed_pattern`.
