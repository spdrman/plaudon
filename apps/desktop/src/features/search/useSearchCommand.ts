/**
 * Hook for search_library command.
 */
import { useCommandQuery } from "../../ipc/useCommand";

export interface UseSearchLibraryOptions {
  query: string;
  limit?: number;
  enabled?: boolean;
}

export function useSearchLibrary({ query, limit = 50, enabled = true }: UseSearchLibraryOptions) {
  return useCommandQuery(
    "search_library",
    { query, cursor: null, limit },
    { enabled: enabled && query.length > 0, deps: [query, limit] },
  );
}
