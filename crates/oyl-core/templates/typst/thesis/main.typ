// A thesis in several files: this one sets the look and holds the front
// matter; each chapter lives in chapters/ and is included below.

#let title = "{{title}}"
#let author = "{{author}}"
#let date = "{{date}}"
#let degree = "A thesis submitted for the degree of Doctor of Philosophy"
#let institution = "Your University"
#let accent = rgb("#8c5212")

#set document(title: title, author: author)
#set page(paper: "a4", margin: (x: 2.8cm, top: 3cm, bottom: 3.2cm))
#set text(font: "Libertinus Serif", size: 11.5pt, lang: "en")
#set par(justify: true, leading: 0.72em, spacing: 0.72em, first-line-indent: 1.5em)

#set heading(numbering: "1.1")
#show heading: set text(weight: "semibold")
#show heading: set block(above: 2em, below: 1em)
#show heading.where(level: 2): set text(size: 13pt)
#show heading.where(level: 3): set text(size: 11.5pt, style: "italic", weight: "regular")

// Chapters start on a new page with a large title.
#show heading.where(level: 1): it => {
  pagebreak(weak: true)
  set block(above: 0pt, below: 0pt)
  v(2.5cm)
  if it.numbering != none {
    block(text(size: 13pt, fill: luma(110), weight: "regular")[
      Chapter #context counter(heading).display("1")
    ])
    v(0.9em)
  }
  block(text(size: 24pt, it.body))
  v(1.6cm)
}

#set math.equation(numbering: "(1)")
#show figure: set block(spacing: 1.6em)
#show figure.caption: set text(size: 10pt)
#show figure.where(kind: table): set figure.caption(position: top)
#show link: set text(fill: accent)
#show ref: set text(fill: accent)
#show cite: set text(fill: accent)
#show outline.entry.where(level: 1): set block(above: 1.1em)
#show outline.entry.where(level: 1): strong

// Title page
#page(numbering: none, align(center + horizon)[
  #text(size: 26pt, weight: "semibold", title)
  #v(2cm)
  #text(size: 14pt, author)
  #v(4cm)
  #text(size: 11.5pt, degree) \
  #v(0.3em)
  #text(size: 11.5pt, institution) \
  #v(0.3em)
  #text(size: 11pt, fill: luma(100), date)
])

// Front matter, numbered i, ii, iii …
#set page(numbering: "i")
#counter(page).update(1)

#heading(numbering: none, outlined: false)[Abstract]

Summarize the question, the approach and the findings of the thesis in one
page. Write it last.

#outline(title: "Contents", indent: auto)

// Main matter, numbered 1, 2, 3 …
#set page(numbering: "1")
#counter(page).update(1)

#include "chapters/introduction.typ"
#include "chapters/background.typ"
#include "chapters/conclusion.typ"

#bibliography("refs.bib", title: "Bibliography", style: "chicago-author-date")
