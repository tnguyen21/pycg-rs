class One:
    def method(self):
        return 1


class Two:
    def method(self):
        return 2


class Three:
    def method(self):
        return 3


def choose(flag):
    if flag == 1:
        return One()
    if flag == 2:
        return Two()
    return Three()


def caller(flag):
    target = choose(flag)
    return target.method()
