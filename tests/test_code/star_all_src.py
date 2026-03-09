"""Source module with explicit ``__all__`` for star-import soundness tests.

``__all__`` lists exactly two names:
- ``public_exported``   — public name, in __all__
- ``_special_exported`` — private name, but *explicitly* in __all__

The other two names must NOT be injected by ``from star_all_src import *``:
- ``public_not_exported``  — public but absent from __all__
- ``_private_not_exported`` — private and absent from __all__
"""

__all__ = ['public_exported', '_special_exported']


def public_exported():
    """Exported via __all__."""
    pass


def _special_exported():
    """Private but explicitly listed in __all__."""
    pass


def public_not_exported():
    """Public but NOT listed in __all__ — must not be importable via star."""
    pass


def _private_not_exported():
    """Private and NOT in __all__ — must not be importable via star."""
    pass
