"""Small VT screen reader for assertions against Ratatui's differential output."""

import codecs
import re
import unicodedata


class Screen:
    def __init__(self, rows, columns):
        self.decoder = codecs.getincrementaldecoder("utf-8")("replace")
        self.pending = ""
        self.x = self.y = 0
        self.resize(rows, columns)

    def resize(self, rows, columns):
        self.rows, self.columns = rows, columns
        self.cells = [[" "] * columns for _ in range(rows)]
        self.x = self.y = 0

    def text(self):
        return "\n".join("".join(row) for row in self.cells)

    def feed(self, data):
        self.pending += self.decoder.decode(data)
        while self.pending:
            if self.pending.startswith("\x1b["):
                match = re.match(r"\x1b\[([0-?]*)([ -/]*)([@-~])", self.pending)
                if match is None:
                    break
                self.csi(match[1], match[3])
                self.pending = self.pending[match.end():]
                continue
            if self.pending.startswith("\x1b]"):
                match = re.search(r"\x07|\x1b\\", self.pending)
                if match is None:
                    break
                self.pending = self.pending[match.end():]
                continue
            if self.pending[0] == "\x1b":
                if len(self.pending) < 2:
                    break
                self.pending = self.pending[2:]
                continue
            character, self.pending = self.pending[0], self.pending[1:]
            if character == "\r":
                self.x = 0
            elif character == "\n":
                self.y = min(self.rows - 1, self.y + 1)
            elif character == "\b":
                self.x = max(0, self.x - 1)
            elif character == "\t":
                self.x = min(self.columns, (self.x // 8 + 1) * 8)
            elif character >= " " and character != "\x7f":
                width = 0 if unicodedata.combining(character) else (2 if unicodedata.east_asian_width(character) in "WF" else 1)
                if width == 0 and self.x:
                    self.cells[self.y][min(self.x, self.columns) - 1] += character
                elif self.x < self.columns:
                    self.cells[self.y][self.x] = character
                    if width == 2 and self.x + 1 < self.columns:
                        self.cells[self.y][self.x + 1] = ""
                self.x = min(self.columns, self.x + width)

    def csi(self, parameters, command):
        if parameters.startswith("?"):
            if parameters == "?1049" and command == "h":
                self.resize(self.rows, self.columns)
            return
        values = [int(value or 0) for value in parameters.split(";")]
        amount = values[0] or 1
        if command in "Hf":
            self.y = max(0, min(self.rows - 1, (values[0] or 1) - 1))
            self.x = max(0, min(self.columns - 1, ((values[1] if len(values) > 1 else 1) or 1) - 1))
        elif command == "G":
            self.x = min(self.columns - 1, amount - 1)
        elif command == "A":
            self.y = max(0, self.y - amount)
        elif command == "B":
            self.y = min(self.rows - 1, self.y + amount)
        elif command == "C":
            self.x = min(self.columns - 1, self.x + amount)
        elif command == "D":
            self.x = max(0, self.x - amount)
        elif command == "J" and values[0] in (2, 3):
            self.cells = [[" "] * self.columns for _ in range(self.rows)]
        elif command == "K":
            start, end = ((0, self.columns) if values[0] == 2 else
                          (0, self.x + 1) if values[0] == 1 else (self.x, self.columns))
            self.cells[self.y][start:end] = [" "] * (end - start)
