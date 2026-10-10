// The adapter that the grader starts for a case: it posts the grader's
// message to the worker that `grade.sh` runs in workerd, and prints its answer.
// It runs without the HTTP_PROXY that the grader sets for a host it cannot
// serve, as the worker's address is not one.

const response = await fetch(process.env.WORKER_URL!, {
  method: "POST",
  body: await Bun.stdin.text(),
});
const text = await response.text();
if (!response.ok) {
  console.error(text);
  process.exit(2);
}
console.log(text);
