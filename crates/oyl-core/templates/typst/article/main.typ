// The lines up to the title block give the paper its look.
// The writing starts at "Introduction".

#let title = "{{title}}"
#let author = "{{author}}"
#let date = "{{date}}"
#let accent = rgb("#8c5212")

#set document(title: title, author: author)
#set page(
  paper: "a4",
  margin: (x: 2.6cm, top: 2.8cm, bottom: 3cm),
  numbering: "1",
)
#set text(font: "Libertinus Serif", size: 11pt, lang: "en")
#set par(justify: true, leading: 0.68em, spacing: 0.68em, first-line-indent: 1.3em)

#set heading(numbering: "1.1")
#show heading: set text(weight: "semibold")
#show heading: set block(above: 1.9em, below: 1em)
#show heading.where(level: 1): set text(size: 13pt)
#show heading.where(level: 2): set text(size: 11.5pt)
#show heading.where(level: 3): set text(size: 11pt, style: "italic", weight: "regular")

#set math.equation(numbering: "(1)")
#show figure: set block(spacing: 1.6em)
#show figure.caption: set text(size: 9.5pt)
#show figure.where(kind: table): set figure.caption(position: top)
#show link: set text(fill: accent)
#show ref: set text(fill: accent)
#show cite: set text(fill: accent)

// Title block
#align(center)[
  #v(0.8cm)
  #text(size: 21pt, weight: "semibold", title)
  #v(0.7em)
  #text(size: 12pt, author)
  #v(0.15em)
  #text(size: 10pt, fill: luma(100), date)
]
#v(1.6em)

#pad(x: 2.5em)[
  #set text(size: 9.5pt)
  #set par(first-line-indent: 0pt)
  #align(center, text(size: 8.5pt, weight: "semibold", tracking: 0.12em)[ABSTRACT])
  #v(0.3em)
  One paragraph on what this paper does, how, and what it finds. Write it
  last, when you know what the paper says.
]
#v(1em)

= Introduction

Start here. Oyl compiles the paper as you type, so the pages follow your
words. Cite a source from `refs.bib` by its key: programs, like papers,
should be written for people first @knuth1984. The list of references at the
end fills in by itself.

Ask the agent to draft, tighten or restructure any part of the paper. It
edits these files directly, and every change is kept in the history.

= Method <sec:method>

Equations are numbered and can be referred to by their label:

$ cal(L)(theta) = - sum_(i=1)^n log p_theta (y_i | x_i) $ <eq:loss>

Minimizing @eq:loss over $theta$ fits the model to the $n$ examples.

= Results

@tab:results shows how a table looks. Images go in the `figures/` folder and
are placed the same way, with `#figure` and a caption.

#figure(
  table(
    columns: 3,
    align: (left, right, right),
    stroke: none,
    inset: (x: 0.8em, y: 0.45em),
    table.hline(stroke: 0.8pt),
    table.header([*Method*], [*Accuracy (%)*], [*Time (s)*]),
    table.hline(stroke: 0.4pt),
    [Method A], [71.2], [3.4],
    [Method B], [84.9], [2.1],
    table.hline(stroke: 0.8pt),
  ),
  caption: [A table with a caption, numbered and referred to by its label.],
) <tab:results>

= Conclusion

Say what the paper showed and what comes next.

#bibliography("refs.bib", title: "References", style: "chicago-author-date")
