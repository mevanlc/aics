You:
I'm creating a search term preprocessor that supports things like:
```
a AND b OR (c AND (d OR e))
```
I already have the parser that deconstructs this properly.
I want to introduce support for introducing regex:
```
a AND b OR (<c.*z> AND (d OR e))
```
but I need to be able to support:
```
a AND b OR (<c\>.*z> AND (d OR e))
```
yields literal regex right before the RE engine gets it:
```
c>.*z
```
but also:
```
a AND b OR (<c\\>.*z> AND (d OR e))
```
is likely an error, either because only whitespace or EOS or ) or maybe ( should follow a regex-closing > or because the regex engine rejects:
```
c\
```
due to \ escaping "nothing". Yet
```
a AND b OR (<c\\\>.*z> AND (d OR e))
```
delivers
```
c\\>.*z
```
to the regex engine, which is a valid regex:
    c followed by backslash followed by greater-than followed by 0-to-many-characters-that-wildcard-.-matches followed by z


ChatGPT:
Yes - the clean way to define this is in terms of the parity of the consecutive backslashes immediately preceding `>`.

The important distinction is that you have two languages layered together:

1. your search-expression syntax, where `<...>` delimits a regex;
2. the regex syntax itself, where `\` has meaning.

I would make your outer parser interfere with regex backslashes only when necessary to escape the outer `>` delimiter.

So for every candidate `>` while scanning a regex, count the immediately preceding run of backslashes:

| Source around `>` | Backslashes | Meaning      |
| ----------------- | ----------: | ------------ |
| `foo>`            |           0 | closes regex |
| `foo\>`           |           1 | literal `>`  |
| `foo\\>`          |           2 | closes regex |
| `foo\\\>`         |           3 | literal `>`  |
| `foo\\\\>`        |           4 | closes regex |
| `foo\\\\\>`       |           5 | literal `>`  |

In other words:

```text
even number of preceding \  => > is delimiter
odd number of preceding \   => > is escaped
```

And when `>` is escaped, remove exactly one backslash-the one belonging to your wrapper syntax-and preserve all the others for the regex engine.

That gives exactly:

```text
<c\>.*z>
```

outer parser produces:

```text
c>.*z
```

while:

```text
<c\\\>.*z>
```

produces:

```text
c\\>.*z
```

which the regex engine interprets as roughly:

```text
c
\\    literal backslash
>
.*
z
```

### One point I'd change in your interpretation

This:

```text
<c\\>.*z>
```

should indeed be a search-language syntax error, but not necessarily because the regex engine receives:

```text
c\
```

If you're preserving regex syntax as above, the first `>` is unescaped because it has two preceding backslashes, so it terminates the regex:

```text
<c\\>
```

and the regex engine receives:

```text
c\\
```

which is normally a perfectly valid regex meaning `c` followed by a literal backslash.

The problem is what comes afterward:

```text
.*z>
```

Your expression parser is now looking at garbage immediately following a completed regex token.

So I'd reject it at the search-expression lexer/parser level, before regex compilation becomes relevant.

For example, after a regex token, the legal next thing might be:

```text
whitespace
EOS
)
AND
OR
```

depending on how your grammar handles token boundaries.

I probably wouldn't allow `(` immediately afterward unless your grammar has implicit operators:

```text
<foo>(bar)
```

Otherwise adjacency should simply be illegal.

### Scanner implementation

You don't actually need a special regex to lex this. A little state machine is much less error-prone:

```text
on '<':
    begin regex

while inside regex:
    if current char != '>':
        consume it
        continue

    count consecutive '\' immediately before this '>'

    if count is even:
        this '>' closes the regex
    else:
        this '>' is payload
        remove one '\' from the accumulated payload
        append '>'
```

Or, even easier when scanning left-to-right, maintain whether the current character is escaped based on the backslash run.

The nice property is that arbitrary depths work naturally:

```text
\>        -> >
\\\>      -> \\>
\\\\\>    -> \\\\>
```

where each odd-backslash case sacrifices one `\` to the outer language and leaves the remaining even number for the regex language.

This is essentially the same rule you encounter parsing things like quoted strings: a delimiter is escaped iff the immediately preceding run of escape characters has odd length. It avoids trying to decide what each individual backslash "means" prematurely.

