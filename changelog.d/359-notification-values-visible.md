---
kind: security
---
Pull request titles, model summaries, finding titles and paths, notes and logins in notifications and in the review queue stay on one line and show each bidi control and zero-width character as `<U+XXXX>`, so such a value can no longer forge a message line or display differently from what it holds; an excerpt cut never splits a `<U+XXXX>`.
