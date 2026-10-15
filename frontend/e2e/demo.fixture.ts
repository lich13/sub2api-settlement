import { spawn, type ChildProcess } from "node:child_process";
import { access } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { expect, test as base } from "@playwright/test";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const readyUrlPattern = /(http:\/\/127\.0\.0\.1:\d+\/settlement\/assets\/__qa__\/index\.html)/;

export type DemoServer = {
  origin: string;
};

async function stopProcess(child: ChildProcess) {
  if (child.exitCode !== null || child.signalCode !== null) return;

  const exited = new Promise<void>((resolveExit) => {
    child.once("exit", () => resolveExit());
  });
  child.kill("SIGTERM");

  let timer: ReturnType<typeof setTimeout> | undefined;
  const graceful = await Promise.race([
    exited.then(() => true),
    new Promise<boolean>((resolveTimeout) => {
      timer = setTimeout(() => resolveTimeout(false), 10_000);
    }),
  ]);
  if (timer) clearTimeout(timer);
  if (graceful) return;

  child.kill("SIGKILL");
  await exited;
  throw new Error("The isolated demo did not stop after SIGTERM; SIGKILL was required");
}

async function startDemo(): Promise<DemoServer & { stop: () => Promise<void> }> {
  const binary = resolve(repoRoot, "target/debug/settlement-tools");
  await access(binary);

  const child = spawn(
    binary,
    ["demo", "--port", "0", "--netting-fixtures", "--fail-delete-once"],
    {
      cwd: repoRoot,
      env: {
        PATH: process.env.PATH ?? "/usr/bin:/bin:/usr/sbin:/sbin",
        TMPDIR: process.env.TMPDIR ?? "/tmp",
      },
      stdio: ["ignore", "pipe", "pipe"],
    },
  );

  let stdout = "";
  child.stdout?.setEncoding("utf8");
  child.stderr?.resume();

  try {
    const readyUrl = await new Promise<string>((resolveReady, rejectReady) => {
      const timeout = setTimeout(() => {
        cleanup();
        rejectReady(new Error("The isolated demo did not report its ready URL within 45 seconds"));
      }, 45_000);

      const cleanup = () => {
        clearTimeout(timeout);
        child.stdout?.off("data", onStdout);
        child.off("error", onError);
        child.off("exit", onExit);
      };
      const onStdout = (chunk: string) => {
        stdout += chunk;
        const match = stdout.match(readyUrlPattern);
        if (match) {
          cleanup();
          resolveReady(match[1]);
        }
      };
      const onError = (error: NodeJS.ErrnoException) => {
        cleanup();
        rejectReady(new Error(`Unable to start the isolated demo (${error.code ?? "process error"})`));
      };
      const onExit = (code: number | null, signal: NodeJS.Signals | null) => {
        cleanup();
        rejectReady(new Error(`The isolated demo exited before ready (code=${code ?? "none"}, signal=${signal ?? "none"})`));
      };

      child.stdout?.on("data", onStdout);
      child.once("error", onError);
      child.once("exit", onExit);
    });

    return {
      origin: new URL(readyUrl).origin,
      stop: () => stopProcess(child),
    };
  } catch (error) {
    await stopProcess(child);
    throw error;
  }
}

type Fixtures = {
  demo: DemoServer;
};

export const test = base.extend<Fixtures>({
  demo: async ({}, use) => {
    const server = await startDemo();
    try {
      await use(server);
    } finally {
      await server.stop();
    }
  },
});

export { expect };
