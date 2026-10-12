/**
 * Cross-recording search field (spec §3.1, §9.8, §17.5).
 */
import { useCallback, useState, type FormEvent } from "react";
import { TextField } from "../../components/Field";

export interface SearchFieldProps {
  onSearch: (query: string) => void;
  disabled?: boolean;
}

export function SearchField({ onSearch, disabled = false }: SearchFieldProps) {
  const [query, setQuery] = useState("");

  const handleSubmit = useCallback(
    (event: FormEvent) => {
      event.preventDefault();
      onSearch(query);
    },
    [query, onSearch],
  );

  return (
    <form onSubmit={handleSubmit} role="search">
      <TextField
        label="Search across all recordings"
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        disabled={disabled}
        placeholder="Enter search terms..."
        aria-label="Search library"
      />
    </form>
  );
}
