"""User module for privacy-filter star-import tests.

Imports everything from star_private_src via ``from … import *``.
``public_func`` must resolve; ``_private_impl`` must NOT resolve (it was
excluded by the privacy filter and must not be resurrected by wildcard
expansion).
"""
from test_code.star_private_src import *  # noqa: F401,F403


def privacy_checker():
    """Calls one public and one private name from the source module."""
    public_func()    # public, no __all__ → must resolve  # noqa: F821
    _private_impl()  # private, no __all__ → must NOT resolve concretely  # noqa: F821
