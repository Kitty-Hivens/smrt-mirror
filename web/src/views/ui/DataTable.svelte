<script lang="ts" module>
  import {
    createTable,
    tableFeatures,
    rowSortingFeature,
    createSortedRowModel,
    sortFn_alphanumeric,
    columnFilteringFeature,
    globalFilteringFeature,
    createFilteredRowModel,
    filterFn_includesString,
  } from '@tanstack/svelte-table';

  // Stable across the module: row sorting and a global filter, and only those.
  // Global filtering builds on the column-filtering feature, so it is included
  // too. The table owns every slice internally -- callers drive sort through the
  // header handlers and the filter through the controlled `filter` prop.
  const FEATURES = tableFeatures({
    rowSortingFeature,
    sortedRowModel: createSortedRowModel(),
    sortFns: { alphanumeric: sortFn_alphanumeric },
    columnFilteringFeature,
    globalFilteringFeature,
    filteredRowModel: createFilteredRowModel(),
    filterFns: { includesString: filterFn_includesString },
  });

  export type Column<T> = {
    id: string;
    header: string;
    width?: string;
    sortable?: boolean;
    // sort + global-filter accessor; columns without one (actions, icon-only)
    // neither sort nor match a filter
    value?: (row: T) => string | number;
  };
</script>

<script lang="ts" generics="T extends RowData">
  import type { Snippet } from 'svelte';
  import type { RowData } from '@tanstack/svelte-table';
  import { settle } from '../../lib/motion.svelte';

  let {
    data,
    columns,
    row,
    onRow,
    empty,
    filter = '',
  }: {
    data: T[];
    columns: Column<T>[];
    // renders the cells -- the <td>s -- for one row. The <tr> around them is the
    // table's, so every table gets the same row behaviour and the same movement
    // when the list is sorted or filtered, rather than each caller re-deciding.
    row: Snippet<[T]>;
    // What clicking (or Enter/Space on) a row does. Absent leaves rows inert.
    onRow?: (row: T) => void;
    // rendered as the tbody body when there are no rows (a full-width <tr>)
    empty?: Snippet;
    // caller-owned global filter text; the caller renders its own input. Empty
    // string (the default) filters nothing, so a table with no filter box is
    // simply left uncontrolled.
    filter?: string;
  } = $props();

  const tsColumns = $derived(
    columns.map((c) => ({
      id: c.id,
      header: c.header,
      accessorFn: c.value ?? (() => ''),
      enableSorting: c.sortable ?? false,
      enableGlobalFilter: c.value != null,
      sortingFn: 'alphanumeric' as const,
      filterFn: 'includesString' as const,
    })),
  );

  // column widths live on the caller's spec, not in TanStack column meta, so the
  // header can size a <th> without a ColumnMeta module augmentation
  const widthById = $derived(new Map(columns.map((c) => [c.id, c.width])));

  const table = createTable({
    features: FEATURES,
    get data() {
      return data;
    },
    get columns() {
      return tsColumns;
    },
    globalFilterFn: 'includesString',
    get state() {
      return { globalFilter: filter };
    },
  });

  const ariaSort = (s: false | 'asc' | 'desc') =>
    s === 'asc' ? 'ascending' : s === 'desc' ? 'descending' : 'none';
</script>

<div class="tablewrap">
  <table class="dt">
    <thead>
      {#each table.getHeaderGroups() as hg (hg.id)}
        <tr>
          {#each hg.headers as header (header.id)}
            {@const sorted = header.column.getIsSorted()}
            <th
              style={widthById.get(header.column.id)
                ? `width:${widthById.get(header.column.id)}`
                : undefined}
              aria-sort={header.column.getCanSort() ? ariaSort(sorted) : undefined}
            >
              {#if header.column.getCanSort()}
                <button
                  class="dt-th"
                  class:sorted={!!sorted}
                  onclick={header.column.getToggleSortingHandler()}
                >
                  {header.column.columnDef.header}
                  <svg
                    class="dt-caret {sorted || 'none'}"
                    width="8"
                    height="8"
                    viewBox="0 0 10 10"
                    aria-hidden="true"
                  >
                    <path
                      d="M2.5 4 L5 6.5 L7.5 4"
                      fill="none"
                      stroke="currentColor"
                      stroke-width="1.4"
                      stroke-linecap="round"
                      stroke-linejoin="round"
                    />
                  </svg>
                </button>
              {:else}
                {header.column.columnDef.header}
              {/if}
            </th>
          {/each}
        </tr>
      {/each}
    </thead>
    <tbody>
      {#each table.getRowModel().rows as r (r.id)}
        <!-- Sorting and filtering rewrite this list, and the rows that survive are
             the same rows: `settle` moves them to their new place instead of
             letting them teleport, so the eye can follow the one it was reading.
             The keydown guard keeps a link or button inside a cell from firing
             the row's own action on top of its own. -->
        <tr
          animate:settle
          class:clickable={!!onRow}
          role={onRow ? 'button' : undefined}
          tabindex={onRow ? 0 : undefined}
          onclick={onRow ? () => onRow(r.original) : undefined}
          onkeydown={onRow
            ? (e) => {
                if (e.target !== e.currentTarget) return;
                if (e.key === 'Enter' || e.key === ' ') {
                  e.preventDefault();
                  onRow(r.original);
                }
              }
            : undefined}
        >
          {@render row(r.original)}
        </tr>
      {/each}
      {#if table.getRowModel().rows.length === 0 && empty}
        {@render empty()}
      {/if}
    </tbody>
  </table>
</div>

<style>
  /* The row is the table's now, and so is how it reads as one: a whole row that
     does something says so under the pointer and takes focus like anything else
     that can be pressed. */
  tr.clickable {
    cursor: pointer;
  }
  tr.clickable:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  /* the sortable header label is a button; reset it to read as the <th> text it
     replaces, with a caret that reveals sort direction */
  .dt-th {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    background: transparent;
    border: none;
    border-radius: 0;
    box-shadow: none;
    padding: 0;
    margin: 0;
    font: inherit;
    letter-spacing: inherit;
    text-transform: inherit;
    color: inherit;
    cursor: pointer;
  }
  .dt-th:hover {
    color: var(--fg);
    background: transparent;
  }
  /* the glyph is a down chevron; descending keeps it, ascending flips it up,
     unsorted dims it to a neutral hint */
  .dt-caret {
    flex-shrink: 0;
    transition: transform var(--dur-state) var(--ease-out);
  }
  .dt-caret.none {
    opacity: 0.28;
  }
  .dt-caret.asc {
    transform: rotate(180deg);
  }
  .dt-th.sorted {
    color: var(--fg);
  }

  /* A narrow column folds each row into a card instead of scrolling a wide
     table sideways under a thumb. The first cell (what the row is) takes the
     whole line, the rest flow under it, and the actions cell sits at the end
     of the last line. A cell that only adds noise at this width is marked
     `narrow-hide` by the caller, which knows what its columns mean. */
  @container view (max-width: 600px) {
    .dt,
    .dt tbody {
      display: block;
    }
    .dt thead {
      display: none;
    }
    .dt tbody tr {
      display: flex;
      flex-wrap: wrap;
      align-items: center;
      gap: var(--space-1) var(--space-3);
      padding: var(--space-3);
      border-bottom: 1px solid var(--seam);
    }
    .dt tbody tr:last-child {
      border-bottom: none;
    }
    .dt tbody tr:nth-child(even) {
      background: var(--zebra);
    }
    .dt tbody tr:hover {
      background: var(--panel-2);
    }
    .dt :global(tbody td) {
      display: block;
      padding: 0;
      border: none;
      background: none;
    }
    .dt tbody tr:nth-child(even) :global(td),
    .dt tbody tr:hover :global(td) {
      background: none;
    }
    .dt :global(tbody td:first-child) {
      flex-basis: 100%;
      min-width: 0;
    }
    .dt :global(tbody td.actions) {
      margin-left: auto;
    }
    .dt :global(tbody td.narrow-hide) {
      display: none;
    }
    .dt :global(tbody td[colspan]) {
      flex-basis: 100%;
    }
  }
</style>
