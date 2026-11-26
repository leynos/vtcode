"""Metric implementations used to grade evaluation outputs."""
import re
import subprocess

class Metric:
    """Base class for evaluation metrics."""
    def evaluate(self, output, target, **kwargs):
        """Raise NotImplementedError; subclasses must implement scoring."""
        raise NotImplementedError

class ExactMatch(Metric):
    """Metric that passes when the output equals the target."""
    def evaluate(self, output, target, **kwargs):
        """Return True if the stripped, lowercased output equals the target."""
        return output.strip().lower() == target.strip().lower()

class ContainsMatch(Metric):
    """Metric that passes when the target text appears in the output."""
    def evaluate(self, output, target, **kwargs):
        """Return True if the target appears in the output, ignoring case."""
        return target.lower() in output.lower()

class LLMGrader(Metric):
    """Metric that grades outputs with an LLM through the diapason CLI."""
    def __init__(self, provider="openai", model="gpt-5"):
        """Store the grading provider and model name."""
        self.provider = provider
        self.model = model

    def evaluate(self, output, rubric, context=None, scale="binary"):
        """Grade the output against a rubric using the diapason CLI.

        Returns a dict with "result" and "reasoning" keys, plus
        "raw_response" when the CLI call succeeds.
        """
        prompt = f"""You are an impartial judge evaluating the output of a coding agent.

        <rubric>
        {rubric}
        </rubric>

        <output>
        {output}
        </output>

        {f'<context>{context}</context>' if context else ''}

        Grade the output based on the rubric.
        Scale: {scale}

        Think through your reasoning in <thinking> tags, then output the result in <result> tags.
        For binary, use 'correct' or 'incorrect'.
        For scales like 1-5, use the number. Only output the XML tags."""

        # Use diapason to grade
        args = [
            "./target/release/diapason",
            "--provider", "gemini", # Use a cheap provider for grading
            "--model", "gemini-3-flash-preview",
            "ask", prompt
        ]

        try:
            result = subprocess.run(args, capture_output=True, text=True, timeout=60)
            if result.returncode != 0:
                return {"result": "error", "reasoning": f"diapason error: {result.stderr}"}

            text = result.stdout
            thinking = re.search(r'<thinking>(.*?)</thinking>', text, re.S)
            res = re.search(r'<result>(.*?)</result>', text, re.S)

            return {
                "result": res.group(1).strip() if res else "error",
                "reasoning": thinking.group(1).strip() if thinking else "No reasoning provided",
                "raw_response": text
            }
        except Exception as e:
            return {"result": "error", "reasoning": str(e)}

class CodeValidity(Metric):
    """Metric that checks whether the code in an output compiles."""
    def evaluate(self, output, language="python", **kwargs):
        """Return True if the extracted code compiles, False otherwise.

        Only Python is checked; other languages always return False.
        """
        # Heuristic: extract code and try to check syntax
        code = self._extract_code(output)
        if not code:
            return False

        if language == "python":
            try:
                compile(code, "<string>", "exec")
                return True
            except SyntaxError:
                return False
        # Add other languages as needed
        return False

    def _extract_code(self, text):
        """Return the contents of the first python code fence, or None."""
        fence_py = re.compile(r"```python(.*?)```", re.S | re.I)
        m = fence_py.search(text)
        if m:
            return m.group(1).strip()
        return None
