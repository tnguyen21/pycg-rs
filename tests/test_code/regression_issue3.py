# Regression fixture for issue #3: complex / nested comprehensions must not
# crash the analyzer.  The original bug was triggered by list comprehensions
# with multiple iteration variables, nested dict comprehensions, and generator
# expressions used as the iterable of an outer comprehension.


def f():
    return [x for x in range(10)]


def g():
    return [(x, y) for x in range(10) for y in range(10)]


def h(results):
    # Nested list/dict comprehensions with tuple-unpacking patterns and a
    # generator expression as the outermost iterable – the exact pattern that
    # originally caused a crash.
    return [
        (
            [(name, allargs) for name, _, _, allargs, _ in recs],
            {name: inargs for name, inargs, _, _, _ in recs},
            {name: meta for name, _, _, _, meta in recs},
        )
        for recs in (results[key] for key in sorted(results.keys()))
    ]


def set_comp(items):
    return {x * 2 for x in items if x > 0}


def dict_comp(items):
    return {k: v for k, v in items}
