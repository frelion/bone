"""Validation, idempotent mutations, and account queries."""


def add(path, event):
    raise NotImplementedError


def add_many(path, events):
    raise NotImplementedError


def balance(path, account=None):
    raise NotImplementedError
