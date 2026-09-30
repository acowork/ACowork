import { describe, expect, it, vi } from "vitest";

// Locale-independent: t() echoes the key so assertions can pin exact hooks.
vi.mock("../../i18n/useTranslation", () => ({
  useTranslation: () => ({ t: (k: string) => k }),
}));
import { fireEvent, render, screen } from "@testing-library/react";
import { ModelPriorityList } from "./ModelPriorityList";

const options = [
  { key: "deepseek::flash", providerId: "deepseek", modelId: "flash" },
  { key: "ollama::qwen", providerId: "ollama", modelId: "qwen" },
  { key: "openai::mini", providerId: "openai", modelId: "mini" },
];
const names = new Map([
  ["deepseek", "DeepSeek"],
  ["ollama", "Ollama"],
  ["openai", "OpenAI"],
]);

describe("ModelPriorityList", () => {
  it("emits the reordered list when a row moves down", () => {
    const onChange = vi.fn();
    render(
      <ModelPriorityList
        items={[
          { provider_id: "deepseek", model_id: "flash" },
          { provider_id: "ollama", model_id: "qwen" },
        ]}
        options={options}
        providerNameById={names}
        onChange={onChange}
      />,
    );
    // First row's "move down" button swaps the two entries.
    fireEvent.click(screen.getAllByTitle("common.modelList.moveDown")[0]);
    expect(onChange).toHaveBeenCalledWith([
      { provider_id: "ollama", model_id: "qwen" },
      { provider_id: "deepseek", model_id: "flash" },
    ]);
  });

  it("removes the clicked row", () => {
    const onChange = vi.fn();
    render(
      <ModelPriorityList
        items={[
          { provider_id: "deepseek", model_id: "flash" },
          { provider_id: "ollama", model_id: "qwen" },
        ]}
        options={options}
        providerNameById={names}
        onChange={onChange}
      />,
    );
    fireEvent.click(screen.getAllByTitle("common.modelList.remove")[0]);
    expect(onChange).toHaveBeenCalledWith([
      { provider_id: "ollama", model_id: "qwen" },
    ]);
  });

  it("add dropdown only offers unpicked options", () => {
    render(
      <ModelPriorityList
        items={[{ provider_id: "deepseek", model_id: "flash" }]}
        options={options}
        providerNameById={names}
        onChange={vi.fn()}
      />,
    );
    const select = screen.getByRole("combobox");
    const values = Array.from(select.querySelectorAll("option")).map(
      (o) => o.value,
    );
    expect(values).toContain("ollama::qwen");
    expect(values).toContain("openai::mini");
    expect(values).not.toContain("deepseek::flash");
  });

  it("marks persisted entries whose provider was removed as unavailable", () => {
    render(
      <ModelPriorityList
        items={[{ provider_id: "gone", model_id: "model-x" }]}
        options={options}
        providerNameById={names}
        onChange={vi.fn()}
      />,
    );
    expect(screen.getByText(/model-x/).textContent).toContain("common.modelList.unavailable");
  });
});
