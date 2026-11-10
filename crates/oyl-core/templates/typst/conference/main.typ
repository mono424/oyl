// A two-column conference paper. The lines up to the title block give it its
// look; the writing starts at the abstract.

#let title = "{{title}}"
#let author = "{{author}}"
#let affiliation = "Your institution"
#let date = "{{date}}"
#let keywords = ("typesetting", "literate programming", "writing tools")
#let accent = rgb("#8c5212")

#set document(title: title, author: author, keywords: keywords)
#set page(
  paper: "us-letter",
  margin: (x: 1.9cm, top: 2.2cm, bottom: 2.5cm),
  columns: 2,
  numbering: "1",
)
#set columns(gutter: 0.75cm)
#set text(font: "Libertinus Serif", size: 10pt, lang: "en")
#set par(justify: true, leading: 0.55em, spacing: 0.55em, first-line-indent: 1em)

#set heading(numbering: "1.1")
#show heading: set text(size: 10pt, weight: "bold")
#show heading: set block(above: 1.5em, below: 0.8em)
#show heading.where(level: 1): set text(size: 10.5pt)
#show heading.where(level: 1): smallcaps
#show heading.where(level: 3): set text(style: "italic", weight: "regular")

#set math.equation(numbering: "(1)")
#show figure: set block(spacing: 1.3em)
#show figure.caption: set text(size: 9pt)
#show figure.where(kind: table): set figure.caption(position: top)
#show link: set text(fill: accent)
#show ref: set text(fill: accent)
#show cite: set text(fill: accent)

// Title block, across both columns
#place(top + center, float: true, scope: "parent", clearance: 2em)[
  #text(size: 17pt, weight: "bold", title)
  #v(0.6em)
  #text(size: 11pt, author) \
  #text(size: 9.5pt, style: "italic", affiliation) \
  #text(size: 9pt, fill: luma(100), date)
]

#[
  #set text(size: 9pt)
  #set par(first-line-indent: 0pt, spacing: 0.9em)
  *_Abstract_* --- One paragraph on the problem, the approach, and the main
  result, with the number that matters. Write it last.

  *_Keywords_* --- #keywords.join(", ")
]

= Introduction

State the problem and why it matters in the first paragraph. Programs, like
papers, should be written for people first @knuth1984; so should the opening
of a paper.

End the introduction with the contributions:
- the first contribution, in one line;
- the second, with the section that shows it (@sec:approach).

= Approach <sec:approach>

Describe the method precisely enough to reproduce it. Equations are numbered:

$ hat(y) = arg max_y p_theta (y | x) $ <eq:decode>

and referred to by label, as in @eq:decode.

= Evaluation

#figure(
  table(
    columns: 3,
    align: (left, right, right),
    stroke: none,
    inset: (x: 0.6em, y: 0.4em),
    table.hline(stroke: 0.8pt),
    table.header([*Method*], [*Score*], [*Time (s)*]),
    table.hline(stroke: 0.4pt),
    [Method A], [71.2], [3.4],
    [Method B], [84.9], [2.1],
    table.hline(stroke: 0.8pt),
  ),
  caption: [Results on the main benchmark.],
) <tab:main>

@tab:main compares the methods. Images go in the `figures/` folder and are
placed the same way, with `#figure` and a caption.

= Related Work

Place the paper among the closest prior work, citing each by its key.

= Conclusion

Summarize the result and its limits in a few sentences.

#bibliography("refs.bib", title: "References", style: "ieee")
