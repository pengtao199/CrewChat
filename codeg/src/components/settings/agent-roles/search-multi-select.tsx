"use client"

import { useState } from "react"
import { Check, ChevronsUpDown, X } from "lucide-react"
import { useTranslations } from "next-intl"

import { Badge } from "@/components/ui/badge"
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/components/ui/command"
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover"
import { cn } from "@/lib/utils"

export interface SearchMultiSelectOption {
  id: string
  label: string
  description?: string
  badge?: string
}

interface SearchMultiSelectProps {
  options: SearchMultiSelectOption[]
  selected: string[]
  onChange: (next: string[]) => void
  /** Shown in the field while nothing is selected (and when disabled). */
  placeholder: string
  /** No popover, dimmed field — the caller owns the reason (state text). */
  disabled?: boolean
  className?: string
}

/**
 * Searchable multi-select in the house control grammar: a bordered field that
 * keeps the picked items as chips, opening into a searchable list. The trigger
 * is a `div` rather than a button so each chip can own a real remove button
 * without nesting interactive elements.
 */
export function SearchMultiSelect({
  options,
  selected,
  onChange,
  placeholder,
  disabled = false,
  className,
}: SearchMultiSelectProps) {
  const t = useTranslations("AgentRoles")
  const [open, setOpen] = useState(false)

  const labelOf = (id: string) =>
    options.find((option) => option.id === id)?.label ?? id

  const toggle = (id: string) => {
    onChange(
      selected.includes(id)
        ? selected.filter((item) => item !== id)
        : [...selected, id]
    )
  }

  return (
    <Popover
      open={open && !disabled}
      onOpenChange={(next) => setOpen(disabled ? false : next)}
    >
      <PopoverTrigger asChild>
        <div
          role="button"
          aria-expanded={open && !disabled}
          aria-haspopup="listbox"
          tabIndex={disabled ? -1 : 0}
          onKeyDown={(event) => {
            if (disabled) return
            if (event.key === "Enter" || event.key === " ") {
              event.preventDefault()
              setOpen(!open)
            }
          }}
          className={cn(
            "flex min-h-9 w-full flex-wrap items-center gap-1 rounded-md border border-input bg-transparent px-2 py-1.5 text-xs shadow-xs transition-[color,box-shadow] outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50",
            disabled ? "cursor-not-allowed opacity-50" : "cursor-pointer",
            className
          )}
        >
          {selected.length === 0 ? (
            <span className="text-muted-foreground">{placeholder}</span>
          ) : (
            selected.map((id) => (
              <Badge
                key={id}
                variant="secondary"
                className="gap-1 pr-1 text-2xs font-normal"
              >
                <span className="max-w-52 truncate">{labelOf(id)}</span>
                {disabled ? null : (
                  <button
                    type="button"
                    tabIndex={-1}
                    aria-label={t("removeSelection", { name: labelOf(id) })}
                    className="rounded-sm opacity-60 hover:opacity-100"
                    onClick={(event) => {
                      event.stopPropagation()
                      toggle(id)
                    }}
                  >
                    <X className="h-3 w-3" />
                  </button>
                )}
              </Badge>
            ))
          )}
          <ChevronsUpDown className="ml-auto h-3.5 w-3.5 shrink-0 opacity-50" />
        </div>
      </PopoverTrigger>
      <PopoverContent
        align="start"
        className="w-[var(--radix-popover-trigger-width)] p-0"
      >
        <Command>
          <CommandInput
            placeholder={t("searchPlaceholder")}
            className="h-8 text-xs"
          />
          <CommandList>
            <CommandEmpty className="py-3 text-center text-xs text-muted-foreground">
              {t("noResults")}
            </CommandEmpty>
            <CommandGroup>
              {options.map((option) => {
                const isSelected = selected.includes(option.id)
                return (
                  <CommandItem
                    key={option.id}
                    value={`${option.label} ${option.id} ${option.description ?? ""}`}
                    onSelect={() => toggle(option.id)}
                    className="items-start gap-2"
                  >
                    <Check
                      className={cn(
                        "mt-0.5 size-3.5 shrink-0",
                        isSelected ? "opacity-100" : "opacity-0"
                      )}
                    />
                    <span className="min-w-0 flex-1">
                      <span className="flex items-center gap-1.5 text-xs font-medium">
                        <span className="truncate">{option.label}</span>
                        {option.badge ? (
                          <Badge
                            variant="outline"
                            className="shrink-0 text-2xs font-normal"
                          >
                            {option.badge}
                          </Badge>
                        ) : null}
                      </span>
                      {option.description ? (
                        <span className="mt-0.5 block truncate text-2xs text-muted-foreground">
                          {option.description}
                        </span>
                      ) : null}
                    </span>
                  </CommandItem>
                )
              })}
            </CommandGroup>
          </CommandList>
        </Command>
      </PopoverContent>
    </Popover>
  )
}
