"""Accuracy fixture: additional precision / expected-absence tests.

Covers patterns where a naive analysis might over-approximate:
- Branch join with an unrelated third class
- Diamond inheritance MRO resolution
- Override vs inherited method isolation
- Nested function scope isolation
- Return-type isolation across unrelated factories
"""


# --- Branch join: unrelated third class should not leak ---

class Red:
    def color(self):
        pass

class Blue:
    def color(self):
        pass

class Green:
    """Never assigned to x -- should not appear in edges."""
    def color(self):
        pass

def pick_color(flag):
    if flag:
        x = Red()
    else:
        x = Blue()
    x.color()


# --- Diamond inheritance MRO ---

class Base:
    def act(self):
        pass

class Left(Base):
    def act(self):
        pass

class Right(Base):
    def act(self):
        pass

class Diamond(Left, Right):
    pass

def call_diamond_act():
    """Diamond MRO is [Diamond, Left, Right, Base]. Should resolve to Left.act."""
    d = Diamond()
    d.act()


# --- Inherited vs overridden method ---

class Animal:
    def breathe(self):
        pass
    def swim(self):
        pass

class Bird(Animal):
    def fly(self):
        pass

class Fish(Animal):
    def swim(self):
        pass

def call_bird_breathe():
    """Bird inherits breathe from Animal. Should NOT reach Fish or Fish.swim."""
    b = Bird()
    b.breathe()

def call_fish_swim():
    """Fish overrides swim. Should reach Fish.swim, NOT Animal.swim."""
    f = Fish()
    f.swim()


# --- Nested function scope isolation ---

def outer_with_helper():
    def _helper():
        pass
    _helper()

def unrelated_caller():
    """Calls outer_with_helper. Should NOT directly reach _helper."""
    outer_with_helper()


# --- Return-type isolation across factories ---

class Foo:
    def method(self):
        pass

class Bar:
    def method(self):
        pass

def make_foo():
    return Foo()

def make_bar():
    return Bar()

def use_foo():
    """make_foo().method() should reach Foo.method, NOT Bar.method."""
    obj = make_foo()
    obj.method()

def use_bar():
    """make_bar().method() should reach Bar.method, NOT Foo.method."""
    obj = make_bar()
    obj.method()
