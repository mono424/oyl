// Notes: loose structure, quick to write in. The lines up to the title give
// them their look.

#let title = "{{title}}"
#let author = "{{author}}"
#let date = "{{date}}"
#let accent = rgb("#8c5212")

#set document(title: title, author: author)
#set page(
  paper: "a4",
  margin: (x: 2.4cm, top: 2.6cm, bottom: 2.6cm),
  numbering: "1",
  header: context {
    if counter(page).get().first() > 1 {
      set text(size: 8.5pt, fill: luma(120))
      title
      h(1fr)
      date
    }
  },
)
#set text(font: "Libertinus Serif", size: 11pt, lang: "en")
#set par(leading: 0.7em, spacing: 1.15em)

#show heading: set text(weight: "semibold")
#show heading: set block(above: 1.7em, below: 0.8em)
#show heading.where(level: 1): it => block(
  width: 100%,
  inset: (bottom: 0.35em),
  stroke: (bottom: 0.5pt + luma(190)),
  text(size: 13.5pt, it.body),
)
#show heading.where(level: 2): set text(size: 11.5pt)
#set list(marker: ([•], [–]), indent: 0.4em)
#set enum(indent: 0.4em)
#show link: set text(fill: accent)
#show ref: set text(fill: accent)
#show cite: set text(fill: accent)

#text(size: 22pt, weight: "semibold", title)
#v(0.55em)
#text(fill: luma(110))[#author #h(0.6em) · #h(0.6em) #date]
#v(1.2em)

= Ideas

- Write things down as they come; structure can wait.
- One idea per bullet, so it is easy to move later.
  - Sub-points go one level in.

= Reading

Keep track of what you read and what it gave you. Programs, like papers,
should be written for people first @knuth1984. Every source cited from
`refs.bib` appears in the list at the end.

= Next steps

+ Turn the strongest idea into a paragraph.
+ Ask the agent to find the related work and add it to `refs.bib`.
+ Start a paper from these notes when the outline holds.

#bibliography("refs.bib", title: "Sources", style: "chicago-author-date")
