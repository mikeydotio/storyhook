"""Find exact cycle members in blocker-to-dependent edges without recursion."""

import sys


def cycle_members(edges):
    """Return the sorted IDs belonging to a directed cycle, including self-loops."""
    forward, reverse = {}, {}
    for source, target in edges:
        forward.setdefault(source, set()).add(target)
        forward.setdefault(target, set())
        reverse.setdefault(target, set()).add(source)
        reverse.setdefault(source, set())

    # Finish every descendant before its ancestor; explicit iterators keep long
    # dependency chains independent of Python's recursion limit.
    seen, finished = set(), []
    for root in forward:
        if root in seen:
            continue
        seen.add(root)
        stack = [(root, iter(forward[root]))]
        while stack:
            node, neighbors = stack[-1]
            try:
                neighbor = next(neighbors)
            except StopIteration:
                finished.append(node)
                stack.pop()
                continue
            if neighbor not in seen:
                seen.add(neighbor)
                stack.append((neighbor, iter(forward[neighbor])))

    seen, members = set(), []
    for root in reversed(finished):
        if root in seen:
            continue
        component, pending = [], [root]
        seen.add(root)
        while pending:
            node = pending.pop()
            component.append(node)
            for neighbor in reverse[node]:
                if neighbor not in seen:
                    seen.add(neighbor)
                    pending.append(neighbor)
        # A singleton component needs a self-edge to have a positive-length cycle.
        if len(component) > 1 or root in forward[root]:
            members.extend(component)
    return sorted(members)


def main():
    """Read TSV edges from stdin and print each cycle member once."""
    edges = []
    for number, line in enumerate(sys.stdin, 1):
        if not line.strip():
            continue
        fields = line.rstrip("\n").split("\t")
        if len(fields) != 2 or not all(fields):
            raise ValueError(f"invalid blocking edge on line {number}")
        edges.append(tuple(fields))
    for member in cycle_members(edges):
        print(member)


if __name__ == "__main__":
    main()
