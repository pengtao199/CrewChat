import Foundation

struct CoreFolderDetail: Decodable, Sendable {
    let id: Int
    let name: String
    let path: String
    let alias: String?
    let sortOrder: Int

    enum CodingKeys: String, CodingKey {
        case id
        case name
        case path
        case alias
        case sortOrder = "sort_order"
    }

    var domain: Workspace {
        Workspace(id: id, name: name, alias: alias, path: path, sortOrder: sortOrder)
    }
}

struct CorePathBody: Encodable {
    let path: String
}

struct CoreFolderIDBody: Encodable {
    let folderID: Int

    enum CodingKeys: String, CodingKey {
        case folderID = "folder_id"
    }
}
