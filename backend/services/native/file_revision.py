from os import stat_result


def exact_stat_revision(file_size_bytes: int, file_mtime_ns: int) -> str:
    return f"{file_size_bytes}:{file_mtime_ns}"


def revision_from_stat(stat: stat_result) -> str:
    return exact_stat_revision(stat.st_size, stat.st_mtime_ns)
