// A reader for the XML documents S3 answers with: elements, text, character
// references and CDATA. It skips the declaration, comments, processing
// instructions and attributes, and drops namespace prefixes.

export interface XmlElement {
  name: string;
  children: XmlElement[];
  /** The text directly inside the element, references resolved. */
  text: string;
}

export class XmlError extends Error {
  override name = "XmlError";
}

const namedEntities: Record<string, string> = {
  amp: "&",
  lt: "<",
  gt: ">",
  quot: '"',
  apos: "'",
};

function resolveReferences(text: string): string {
  return text.replace(/&(#x[0-9a-fA-F]+|#[0-9]+|[a-zA-Z]+);/g, (whole, reference: string) => {
    if (reference.startsWith("#x")) {
      return String.fromCodePoint(parseInt(reference.slice(2), 16));
    }
    if (reference.startsWith("#")) {
      return String.fromCodePoint(parseInt(reference.slice(1), 10));
    }
    const named = namedEntities[reference];
    if (named === undefined) {
      throw new XmlError(`unknown entity ${whole}`);
    }
    return named;
  });
}

// A start tag, whose attribute values may hold `>`.
const startTag = /<([^\s/>]+)(?:\s+[^\s=/>]+\s*=\s*(?:"[^"]*"|'[^']*'))*\s*(\/?)>/y;

function localName(name: string): string {
  const colon = name.indexOf(":");
  return colon < 0 ? name : name.slice(colon + 1);
}

/** Reads a document and returns its root element. */
export function parseXml(document: string): XmlElement {
  const stack: XmlElement[] = [];
  let root: XmlElement | undefined;
  let at = 0;
  const skipPast = (end: string) => {
    const found = document.indexOf(end, at);
    if (found < 0) throw new XmlError(`no ${end}`);
    at = found + end.length;
  };
  while (at < document.length) {
    const open = document.indexOf("<", at);
    const text = document.slice(at, open < 0 ? document.length : open);
    if (stack.length > 0) {
      stack[stack.length - 1]!.text += resolveReferences(text);
    } else if (text.trim() !== "") {
      throw new XmlError("text outside the root element");
    }
    if (open < 0) break;
    at = open;
    if (document.startsWith("<?", at)) {
      skipPast("?>");
    } else if (document.startsWith("<!--", at)) {
      skipPast("-->");
    } else if (document.startsWith("<![CDATA[", at)) {
      const end = document.indexOf("]]>", at);
      if (end < 0 || stack.length === 0) throw new XmlError("bad CDATA");
      stack[stack.length - 1]!.text += document.slice(at + 9, end);
      at = end + 3;
    } else if (document.startsWith("<!", at)) {
      skipPast(">");
    } else if (document.startsWith("</", at)) {
      const end = document.indexOf(">", at);
      if (end < 0) throw new XmlError("unclosed end tag");
      const name = localName(document.slice(at + 2, end).trim());
      const element = stack.pop();
      if (element === undefined || element.name !== name) {
        throw new XmlError(`unexpected </${name}>`);
      }
      at = end + 1;
    } else {
      startTag.lastIndex = at;
      const tag = startTag.exec(document);
      if (!tag) throw new XmlError("bad start tag");
      const element: XmlElement = { name: localName(tag[1]!), children: [], text: "" };
      if (stack.length > 0) {
        stack[stack.length - 1]!.children.push(element);
      } else if (root === undefined) {
        root = element;
      } else {
        throw new XmlError("two root elements");
      }
      if (tag[2] !== "/") stack.push(element);
      at += tag[0].length;
    }
  }
  if (root === undefined || stack.length > 0) {
    throw new XmlError("incomplete document");
  }
  return root;
}

export function child(element: XmlElement, name: string): XmlElement | undefined {
  return element.children.find((found) => found.name === name);
}

export function children(element: XmlElement, name: string): XmlElement[] {
  return element.children.filter((found) => found.name === name);
}

/** The text of the first child of `name`, or `undefined` without one. */
export function childText(element: XmlElement, name: string): string | undefined {
  return child(element, name)?.text;
}
