"""Event series over the point lists a `View` hands back."""

from itertools import pairwise


def rises(points, apart=0.0):
    times = []
    last = None

    for at, value in points:
        if value > (0 if last is None else last) and (
            not times or at - times[-1] >= apart
        ):
            times.append(at)

        last = value

    return times


def gaps(times):
    return [later - earlier for earlier, later in pairwise(times)]
