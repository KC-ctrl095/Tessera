"""Errors that become JSON error responses."""


class ApiError(Exception):
    def __init__(self, status, message):
        super().__init__(message)
        self.status = status
        self.message = message


def bad_request(message):
    return ApiError(400, message)


def not_found(message):
    return ApiError(404, message)
